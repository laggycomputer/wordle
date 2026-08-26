use anyhow::Context as _;
use core::fmt::Formatter;
use core::fmt::Write as _;
use core::time::Duration;
use indicatif::ProgressStyle;
use itertools::Itertools as _;
use rayon::iter::IndexedParallelIterator as _;
use rayon::iter::IntoParallelRefIterator as _;
use rayon::iter::ParallelIterator as _;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::GuessResult;
use rs_wordle_solver::Guesser as _;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::TryRecvError;
use wordle::WORDS;
use wordle::time;
use wordle::word_bank;
use zarrs::array::Array;
use zarrs::array::ArrayBuilder;
use zarrs::array::ArraySubset;
use zarrs::array::data_type;
use zarrs::storage::ReadableWritableListableStorage;

#[derive(Clone, Copy)]
enum BakeTarget<'o> {
    BaseState,
    AfterResponse(
        &'o MaxScoreGuesser<MaxComboEliminationsScorer>,
        &'o [LetterResult],
    ),
}

struct BakeTargetIdent<'o>(BakeTarget<'o>);

impl BakeTarget<'_> {
    fn ident(&self) -> BakeTargetIdent<'_> {
        BakeTargetIdent(*self)
    }
}

impl core::fmt::Display for BakeTargetIdent<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
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

fn bake_for(
    dirs: &directories::ProjectDirs,
    bank: &WordBank,
    target: BakeTarget<'_>,
) -> anyhow::Result<MaxScoreGuesser<MaxComboEliminationsScorer>> {
    let data_path = dirs.data_dir();

    let store_path = {
        let mut p = data_path.join("");
        write!(p.as_mut_os_string(), "{}.zarr", target.ident())?;
        p
    };

    let store = Arc::new(zarrs::filesystem::FilesystemStore::new(&store_path)?)
        as ReadableWritableListableStorage;

    let store_shape = [WORDS.len() as u64];
    let subset_all = ArraySubset::new_with_shape(vec![WORDS.len() as u64]);
    // could be other fs e.g. permission error but meh
    let (w_store, s_store) = if let Ok(w) = Array::open(store.clone(), "/word") {
        let s = Array::open(store.clone(), "/score")?;
        (w, s)
    } else {
        eprintln!("creating store {}", store_path.display());
        zarrs::group::GroupBuilder::new()
            .build(store.clone(), "/")?
            .store_metadata()?;

        let w = ArrayBuilder::new(store_shape, [1000], data_type::string(), "")
            .build(store.clone(), "/word")?;
        w.store_metadata()?;
        w.store_array_subset(&subset_all, &*WORDS)?;

        let s = ArrayBuilder::new(store_shape, [1000], data_type::int64(), i64::MIN)
            .build(store.clone(), "/score")?;
        s.store_metadata()?;

        (w, s)
    };

    let s_store = Arc::new(s_store);

    let words = w_store.retrieve_array_subset::<Vec<String>>(&subset_all)?;
    let scores = s_store.retrieve_array_subset::<Vec<i64>>(&subset_all)?;

    let bank_todo = {
        words
            .iter()
            .zip(&scores)
            .filter_map(|(w, s)| (*s == i64::MIN).then_some(Arc::from(w.as_str())))
            .collect::<Vec<_>>()
    };

    let mut guesser = match target {
        BakeTarget::BaseState => {
            #[expect(clippy::expect_used, reason = "i read the code man")]
            let scorer =
                MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)
                    .expect("appears to be infallible");
            MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer)
        }
        BakeTarget::AfterResponse(guesser, results) => {
            let mut guesser = guesser.clone();
            let guess = guesser.select_next_guess().context("best next guess")?;
            guesser.update(&GuessResult {
                guess: &guess,
                results: results.to_owned(),
            })?;

            guesser
        }
    };

    let scores = if !bank_todo.is_empty() {
        eprintln!("baking {}...", target.ident());
        time("bake missing words", || {
            let scores = Arc::new(Mutex::new(scores.into_boxed_slice()));

            let loaded_bake_progress = store_shape[0] - bank_todo.len() as u64;
            eprintln!(
                "baked {loaded_bake_progress}/{}; baking {} more...",
                store_shape[0],
                bank_todo.len()
            );

            let bar = indicatif::ProgressBar::new(store_shape[0]);
            bar.set_position(loaded_bake_progress);
            bar.set_style(ProgressStyle::with_template(
                "{wide_bar} {pos}/{len} {per_sec} {elapsed_precise}/{eta_precise} remaining",
            )?);

            let (stop_cron_tx, stop_cron_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let cron = std::thread::spawn({
                let s_store = s_store.clone();
                let scores = scores.clone();
                let bar = bar.clone();
                move || {
                    let subset_all = s_store.subset_all();
                    loop {
                        if matches!(
                            stop_cron_rx.try_recv(),
                            Ok(()) | Err(TryRecvError::Disconnected)
                        ) {
                            break;
                        }

                        bar.tick();
                        match s_store.store_array_subset(&subset_all, &**scores.lock().unwrap()) {
                            Ok(_) => (),
                            Err(e) => bar.println(format!("err storing: {e}")),
                        }

                        std::thread::sleep(Duration::from_secs(1));
                    }

                    match s_store.store_array_subset(&subset_all, &**scores.lock().unwrap()) {
                        Ok(_) => (),
                        Err(e) => bar.println(format!("err storing: {e}")),
                    }
                }
            });
            bar.enable_steady_tick(Duration::from_secs(10));

            bank_todo.par_iter().enumerate().for_each(|(i, w)| {
                let score = guesser.score_word(w);
                scores.lock().unwrap()[i] = score;
                bar.inc(1);
            });

            bar.finish();

            eprintln!(
                "baked guesser is finished in state {} at {}",
                target.ident(),
                store_path.display(),
            );

            stop_cron_tx.send(())?;
            let _ = cron.join();

            let scores = Arc::into_inner(scores)
                .context("we should be the sole holder")?
                .into_inner()?
                .to_vec();

            anyhow::Ok(words.into_iter().map(Arc::from).zip(scores).collect())
        })?
    } else {
        eprintln!("{} was already baked", target.ident());
        words.into_iter().map(Arc::from).zip(scores).collect()
    };

    guesser = guesser.with_scores(&scores);

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
    for result in core::iter::repeat_n(
        [
            LetterResult::NotPresent,
            LetterResult::PresentNotHere,
            LetterResult::Correct,
        ],
        bank.word_length(),
    )
    .multi_cartesian_product()
    {
        bake_for(
            &dirs,
            &bank,
            BakeTarget::AfterResponse(&base_guesser, &result),
        )?;
    }

    Ok(())
}
