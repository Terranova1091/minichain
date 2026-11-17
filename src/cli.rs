use std::{
    fs,
    io::{self, Write},
    path::{PathBuf},
};

use clap::{Parser, Subcommand};
use thiserror::Error;

use crate::mempool::{Mempool, MempoolError};
use crate::models::{
    account::State,
    chain::{BuildError, BuiltBlock, Chain, ChainError, StorageError},
    transaction::Transaction,
};

const CHAIN_FILE: &str = "chain.json";
const PENDING_FILE: &str = "pending.json";

#[derive(Parser)]
#[command(name = "minichain", about = "Учебный блокчейн minichain")]
pub struct Cli {
    #[arg(long, default_value = ".")]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init { #[arg(long)] bits: u32 },
    Send { #[arg(long)] from: String, #[arg(long)] to: String, #[arg(long)] amount: u64 },
    Mine { #[arg(long)] to: String },
    Show { #[arg(long)] height: Option<u64> },
    Verify,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),

    #[error("No block at height {0}")]
    NoSuchBlock(u64),

    #[error("Mining failed: {0}")]
    Mine(String),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    Mempool(#[from] MempoolError),

    #[error(transparent)]
    Build(#[from] BuildError),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

struct Paths {
    data_dir: PathBuf,
    chain: PathBuf,
    pending: PathBuf,
}

impl Paths {
    fn new(data_dir: PathBuf) -> Paths {
        Paths {
            chain: data_dir.join(CHAIN_FILE),
            pending: data_dir.join(PENDING_FILE),
            data_dir,
        }
    }
}

/// Единая точка входа. Время и поток вывода передаются снаружи, поэтому функцию можно тестировать.
pub fn run(cli: Cli, now: u64, out: &mut impl Write) -> Result<(), CliError> {
    let paths = Paths::new(cli.data_dir);
    match cli.command {
        Command::Init { bits } => cmd_init(&paths, bits, out),
        Command::Send { from, to, amount } => cmd_send(&paths, from, to, amount, now, out),
        Command::Mine { to } => cmd_mine(&paths, &to, now, out),
        Command::Show { height } => cmd_show(&paths, height, now, out),
        Command::Verify => cmd_verify(&paths, now, out),
    }
}

fn cmd_init(paths: &Paths, bits: u32, out: &mut impl Write) -> Result<(), CliError> {
    // Существующую цепь не перезаписываем ни при каких условиях.
    if paths.chain.exists() {
        return Err(CliError::AlreadyExists(paths.chain.clone()));
    }
    let chain = Chain::new(bits)?;
    fs::create_dir_all(&paths.data_dir)?;
    chain.save(&paths.chain)?;
    writeln!(out, "initialized chain at {} (bits={bits})", paths.chain.display())?;
    Ok(())
}

/// Следующий свободный nonce отправителя с учётом его транзакций в очереди.
/// Очередь уже прошла revalidate, поэтому каждая транзакция отправителя занимает ровно один номер.
fn next_nonce(state: &State, mempool: &Mempool, from: &str) -> u64 {
    let confirmed = state.accounts.get(from).map_or(0, |account| account.next_nonce);
    let queued = mempool.txs().iter().filter(|tx| tx.from == from).count();
    confirmed.saturating_add(u64::try_from(queued).unwrap_or(u64::MAX))
}

fn cmd_send(
    paths: &Paths,
    from: String,
    to: String,
    amount: u64,
    now: u64,
    out: &mut impl Write,
) -> Result<(), CliError> {
    let chain = Chain::load(&paths.chain, now)?;
    let mut mempool = Mempool::load(&paths.pending)?;

    let dropped = mempool.revalidate(&chain.state);
    if !dropped.is_empty() {
        writeln!(out, "dropped {} stale transaction(s) from the queue", dropped.len())?;
    }

    let nonce = next_nonce(&chain.state, &mempool, &from);
    let tx = Transaction { from, to, amount, nonce };
    mempool.add(tx.clone(), &chain.state)?;
    mempool.save(&paths.pending)?;

    writeln!(out, "queued: {} -> {}, amount {}, nonce {}", tx.from, tx.to, tx.amount, tx.nonce)?;
    Ok(())
}

fn cmd_mine(paths: &Paths, miner: &str, now: u64, out: &mut impl Write) -> Result<(), CliError> {
    let mut chain = Chain::load(&paths.chain, now)?;
    let mut mempool = Mempool::load(&paths.pending)?;

    // 1. Чистим очередь перед сборкой (в том числе остатки после возможного сбоя).
    mempool.revalidate(&chain.state);

    // 2. Собираем и майним блок.
    let BuiltBlock { mut block, skipped } = chain.build_block(miner, mempool.txs(), now)?;
    block.mine().map_err(|e| CliError::Mine(format!("{e:?}")))?;

    let height = block.header.height;
    let hash = block.hash();
    let nonce = block.header.nonce;
    let transfers = block.transactions.len().saturating_sub(1); // без coinbase

    // 3. Сначала сохраняем цепь, потом очередь: при сбое между ними в очереди останутся
    //    уже включённые транзакции, а revalidate на следующем запуске их уберёт.
    chain.add_block(block, now)?;
    chain.save(&paths.chain)?;

    let dropped = mempool.revalidate(&chain.state);
    mempool.save(&paths.pending)?;

    writeln!(
        out,
        "mined block #{height}, hash {hash}, nonce {nonce}, transfers: {transfers}, skipped: {}, dropped: {}",
        skipped.len(),
        dropped.len()
    )?;
    Ok(())
}

fn cmd_show(
    paths: &Paths,
    height: Option<u64>,
    now: u64,
    out: &mut impl Write,
) -> Result<(), CliError> {
    let chain = Chain::load(&paths.chain, now)?;
    match height {
        Some(h) => show_block(&chain, h, out),
        None => show_summary(&chain, paths, out),
    }
}

fn show_summary(chain: &Chain, paths: &Paths, out: &mut impl Write) -> Result<(), CliError> {
    let pending = Mempool::load(&paths.pending)?;

    writeln!(out, "height:   {}", chain.height())?;
    writeln!(out, "blocks:   {}", chain.blocks.len())?;
    if let Some(tip) = chain.tip() {
        writeln!(out, "tip:      {}", tip.hash())?;
        writeln!(out, "bits:     {}", tip.header.bits)?;
    }
    writeln!(out, "pending:  {}", pending.txs().len())?;
    writeln!(out, "balances:")?;
    for (address, account) in &chain.state.accounts {
        writeln!(out, "  {address}: {}", account.balance)?;
    }
    Ok(())
}

fn show_block(chain: &Chain, height: u64, out: &mut impl Write) -> Result<(), CliError> {
    let block = usize::try_from(height)
        .ok()
        .and_then(|index| chain.blocks.get(index))
        .ok_or(CliError::NoSuchBlock(height))?;
    let header = &block.header;

    writeln!(out, "block #{}", header.height)?;
    writeln!(out, "  hash:        {}", block.hash())?;
    writeln!(out, "  version:     {}", header.version)?;
    writeln!(out, "  prev_hash:   {}", header.prev_hash)?;
    writeln!(out, "  merkle_root: {}", header.merkle_root)?;
    writeln!(out, "  timestamp:   {}", header.timestamp)?;
    writeln!(out, "  bits:        {}", header.bits)?;
    writeln!(out, "  nonce:       {}", header.nonce)?;
    writeln!(out, "  transactions ({}):", block.transactions.len())?;
    for tx in &block.transactions {
        let from = if tx.from.is_empty() { "<coinbase>" } else { tx.from.as_str() };
        writeln!(out, "    {from} -> {}, amount {}, nonce {}", tx.to, tx.amount, tx.nonce)?;
    }
    Ok(())
}

fn cmd_verify(paths: &Paths, now: u64, out: &mut impl Write) -> Result<(), CliError> {
    // load перепроверяет всю цепь блок за блоком, ошибка содержит высоту
    let chain = Chain::load(&paths.chain, now)?;
    match chain.tip() {
        Some(tip) => writeln!(out, "OK: {} blocks, tip {}", chain.blocks.len(), tip.hash())?,
        None => writeln!(out, "OK: empty chain")?,
    }
    Ok(())
}