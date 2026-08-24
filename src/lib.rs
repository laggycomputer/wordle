use rs_wordle_solver::WordBank;
use rs_wordle_solver::WordleError;
use std::time::Instant;

pub fn time<F, R>(phase: &str, func: F) -> R
where
    F: FnOnce() -> R,
{
    let start = Instant::now();
    let r = func();
    let duration = start.elapsed();
    println!("{phase} in {}", humantime::format_duration(duration));
    r
}

pub fn words() -> impl Iterator<Item = &'static str> {
    include_str!("../bank.txt").split_ascii_whitespace()
}

pub fn word_bank() -> Result<WordBank, WordleError> {
    WordBank::from_iterator(words())
}
