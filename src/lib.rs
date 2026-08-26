use std::io::Write;
use konst::iter::collect_const;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::WordleError;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

pub fn time<F, R>(phase: &str, func: F) -> R
where
    F: FnOnce() -> R,
{
    let start = Instant::now();
    let r = func();
    let duration = start.elapsed();
    eprintln!("{phase} in {}", humantime::format_duration(duration));
    let _ = std::io::stderr().flush();
    r
}

pub static WORDS: LazyLock<[&str; 14855]> = LazyLock::new(|| {
    #[allow(long_running_const_eval, reason = "halting be damned")]
    let mut c = collect_const!(
        &'static str => konst::string::split(include_str!("../bank.txt"), '\n'),
        filter(|s| !s.is_empty())
    );
    c.sort_unstable();
    c
});

pub static WORDS_ARC: LazyLock<Arc<[Arc<str>]>> = LazyLock::new(|| {
    Arc::from(
        WORDS
            .into_iter()
            .map(Arc::from)
            .collect::<Vec<Arc<str>>>()
            .into_boxed_slice(),
    )
});

pub fn word_bank() -> Result<WordBank, WordleError> {
    WordBank::from_iterator(*WORDS)
}
