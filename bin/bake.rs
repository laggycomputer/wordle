use anyhow::Context as _;
use core::num::NonZero;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use dashmap::DashMap;
use rayon::iter::IntoParallelRefIterator as _;
use rayon::iter::ParallelIterator as _;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use rs_wordle_solver::scorers::WordScorer as _;
use std::sync::Arc;
use wordle::word_bank;
use wordle::words;

fn save_progress(
    project_dirs: &directories::ProjectDirs,
    bank_todo: &[Arc<str>],
    bank_progress: &DashMap<Arc<str>, i64>,
) -> anyhow::Result<()> {
    let data_path = project_dirs.data_dir();
    let bank_todo_path = data_path.join("bank_todo");
    oxicode::encode_to_file(&bank_todo, &bank_todo_path)
        .with_context(|| format!("write bank_progress to {}", bank_todo_path.display()))?;

    let bank_progress_path = data_path.join("bank_progress");
    oxicode::serde::encode_serde_to_file(bank_progress, &bank_progress_path)
        .with_context(|| format!("write bank progress to {}", bank_todo_path.display()))?;

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let bank = word_bank().context("word bank")?;

    let dirs = directories::ProjectDirs::from("com", "laggo", "wordle")
        .context("can't determine storage loc")?;

    let data_path = dirs.data_dir();
    std::fs::create_dir_all(data_path)
        .with_context(|| format!("can't create {}", data_path.display()))?;

    let bank_todo_path = data_path.join("bank_todo");
    let final_path = data_path.join("guesser");

    let (bank_todo, bank_progress) = if std::fs::exists(&bank_todo_path)
        .with_context(|| format!("check if {} exists", bank_todo_path.display()))?
    {
        let todo_str =
            oxicode::decode_from_file::<Vec<Arc<str>>>(&bank_todo_path).with_context(|| {
                format!(
                    "should find valid todo list at {}",
                    bank_todo_path.display()
                )
            })?;

        let bank_progress_path = data_path.join("bank_progress");
        let bank_progress =
            oxicode::serde::decode_serde_from_file::<DashMap<Arc<str>, i64>>(&bank_progress_path)
                .with_context(|| {
                format!(
                    "should find valid partial bake at {}",
                    bank_progress_path.display()
                )
            })?;

        (todo_str, bank_progress)
    } else {
        (
            if !std::fs::exists(&final_path)
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

    if !bank_todo.is_empty() {
        #[expect(clippy::expect_used, reason = "i read the code man")]
        let scorer =
            MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)
                .expect("appears to be infallible");

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
                let _ = save_progress(&dirs, &bank_todo, &bank_progress);
            }
        });

        bar.finish();
        let guesser = MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer)
            .with_scores(&bank_progress.into_iter().collect());

        oxicode::serde::encode_serde_to_file(&guesser, &final_path)
            .with_context(|| format!("save baked guesser to {}", final_path.display()))?;
        eprintln!(
            "saved baked guesser to {} ({} bytes)",
            final_path.display(),
            std::fs::metadata(&final_path)?.len()
        );
    }

    Ok(())
}
