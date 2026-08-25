use anyhow::Context as _;
use core::fmt::Formatter;
use core::fmt::Write as _;
use core::num::NonZero;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use dashmap::DashMap;
use rayon::iter::IntoParallelRefIterator as _;
use rayon::iter::ParallelIterator as _;
use rs_wordle_solver::Guesser as _;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::details::WordRestrictions;
use rs_wordle_solver::scorers::{MaxComboEliminationsScorer, WordScorer};
use rs_wordle_solver::{GuessFrom, GuessResult, WordleError};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use wordle::word_bank;
use wordle::words;

#[derive(Clone, Copy)]
enum BakeTarget<'o> {
    BaseState,
    AfterResponse(&'o MaxComboEliminationsScorer, &'o [LetterResult]),
}

struct BakeTargetIdent<'o>(BakeTarget<'o>);

impl BakeTarget<'_> {
    fn ident(&self) -> BakeTargetIdent<'_> {
        BakeTargetIdent(*self)
    }
}

impl core::fmt::Display for BakeTargetIdent<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            BakeTarget::BaseState => f.write_str("0"),
            BakeTarget::AfterResponse(_, results) => {
                for r in results {
                    f.write_char(match r {
                        LetterResult::Correct => 'g',
                        LetterResult::PresentNotHere => 'y',
                        LetterResult::NotPresent => 'b',
                    })?;
                }

                Ok(())
            }
        }
    }
}

fn save_progress<'bt>(
    project_dirs: &directories::ProjectDirs,
    target: BakeTarget<'bt>,
    bank_progress: &DashMap<Arc<str>, i64>,
) -> anyhow::Result<()> {
    let data_path = project_dirs.data_dir();

    let bank_progress_path = {
        let mut p = data_path.join("");
        write!(p.as_mut_os_string(), "{}", target.ident())?;
        p
    };

    oxicode::serde::encode_serde_to_file(bank_progress, &bank_progress_path)
        .with_context(|| format!("write bank progress to {}", bank_progress_path.display()))?;

    Ok(())
}

struct TakeableScorer<'s, S: WordScorer>(Option<&'s mut S>);

impl<'s, S> TakeableScorer<'s, S>
where
    S: WordScorer,
{
    fn new(scorer: &'s mut S) -> Self {
        Self(Some(scorer))
    }
}

impl<'s, S: WordScorer> WordScorer for TakeableScorer<'s, S> {
    fn update(&mut self, latest_guess: &str, restrictions: &WordRestrictions, possible_words: &[Arc<str>]) -> Result<(), WordleError> {
        self.0.as_mut().unwrap().update(latest_guess, restrictions, possible_words)
    }

    fn score_word(&self, word: &Arc<str>) -> i64 {
        self.0.as_ref().unwrap().score_word(word)
    }
}

fn bake_for(
    dirs: &directories::ProjectDirs,
    bank: &WordBank,
    target: BakeTarget<'_>,
) -> anyhow::Result<MaxScoreGuesser<MaxComboEliminationsScorer>> {
    let data_path = dirs.data_dir();
    let final_path = data_path.join("guesser");

    let partial_suffix = ".partial";
    let bank_progress_path = {
        let mut p = data_path.join("");
        write!(p.as_mut_os_string(), "{}{partial_suffix}", target.ident())?;
        p
    };

    // safety: if the inner slice does not break a codepoint, we have a valid OsStr overall
    let final_path = Path::new(unsafe {
        OsStr::from_encoded_bytes_unchecked(
            &bank_progress_path.as_os_str().as_encoded_bytes()[..(bank_progress_path.as_os_str().len()
                // safety: OsStr is always a superset of 7-bit ASCII
                - { OsStr::from_encoded_bytes_unchecked(partial_suffix.as_bytes()) }.len())],
        )
    });

    let (bank_todo, bank_progress) = if std::fs::exists(&bank_progress_path)
        .with_context(|| format!("check if {} exists", bank_progress_path.display()))?
    {
        let bank_progress =
            oxicode::serde::decode_serde_from_file::<DashMap<Arc<str>, i64>>(&bank_progress_path)
                .with_context(|| {
                    format!(
                        "should find valid partial bake at {}",
                        bank_progress_path.display()
                    )
                })?;

        (
            bank.iter()
                .filter(|w| !bank_progress.contains_key(*w))
                .cloned()
                .collect(),
            bank_progress,
        )
    } else {
        (
            if !std::fs::exists(final_path)
                .with_context(|| format!("check if {} exists", final_path.display()))?
            {
                let mut w = words().map(Arc::from).collect::<Vec<Arc<str>>>();
                w.sort_unstable();
                w
            } else {
                Default::default()
            },
            Default::default(),
        )
    };

    let guesser = if !bank_todo.is_empty() {
        let scorer = match target {
            BakeTarget::BaseState =>
                {
                    #[expect(clippy::expect_used, reason = "i read the code man")]
                    MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)
                        .expect("appears to be infallible")
                }
            BakeTarget::AfterResponse(scorer, results) => {
                let mut scorer = scorer.clone();
                let mut guesser = MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer.clone());
                let guess =
                    guesser
                        .select_next_guess()
                        .context("best next guess")?;
                guesser.update(&GuessResult {
                    guess: &*guess,
                    results: results.to_owned(),
                })?;

                todo!()
            }
        };

        let bar = indicatif::ProgressBar::new((bank_todo.len() + bank_progress.len()) as u64);
        bar.set_position(bank_progress.len() as u64);

        let n_uncommitted = AtomicUsize::new(0);
        let parallelism = std::thread::available_parallelism().map_or(1, NonZero::<usize>::get);

        bank_todo.par_iter().for_each(|w| {
            let score = scorer.score_word(w);
            bank_progress.insert(w.clone(), score);
            bar.inc(1);
            n_uncommitted.fetch_add(1, Ordering::Relaxed);

            let loaded = n_uncommitted.load(Ordering::Acquire);
            if loaded >= parallelism
                && let Ok(_) = n_uncommitted.compare_exchange_weak(
                loaded,
                0,
                Ordering::Release,
                Ordering::Relaxed,
            )
            {
                let _ = save_progress(&dirs, target, &bank_progress);
            }
        });

        bar.finish();
        let guesser = MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer)
            .with_scores(&bank_progress.into_iter().collect());

        oxicode::serde::encode_serde_to_file(&guesser, &final_path)
            .with_context(|| format!("save baked guesser to {}", final_path.display()))?;
        std::fs::remove_file(&bank_progress_path)
            .with_context(|| format!("delete progress file {}", bank_progress_path.display()))?;
        eprintln!(
            "saved baked guesser in state {} to {} ({} bytes)",
            target.ident(),
            final_path.display(),
            std::fs::metadata(final_path)?.len()
        );

        guesser
    } else {
        oxicode::serde::decode_serde_from_file(final_path)
            .with_context(|| format!("load baked guesser from {}", final_path.display()))?
    };

    Ok(guesser)
}

fn main() -> anyhow::Result<()> {
    let bank = word_bank().context("word bank")?;

    let dirs = directories::ProjectDirs::from("com", "laggo", "wordle")
        .context("can't determine storage loc")?;

    let data_path = dirs.data_dir();
    std::fs::create_dir_all(data_path)
        .with_context(|| format!("can't create {}", data_path.display()))?;

    let base_guesser = bake_for(&dirs, &bank, BakeTarget::BaseState)?;

    Ok(())
}
