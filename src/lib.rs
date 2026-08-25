use konst::iter::collect_const;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::WordleError;
use std::sync::LazyLock;
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

pub static WORDS: LazyLock<[&str; 14856]> = LazyLock::new(|| {
    let mut c = collect_const!(
        &'static str => konst::string::split(include_str!("../bank.txt"), '\n'),
    );
    c.sort_unstable();
    c
});

pub fn word_bank() -> Result<WordBank, WordleError> {
    WordBank::from_iterator(*WORDS)
}
