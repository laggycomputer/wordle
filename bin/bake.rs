use anyhow::Context as _;
use anyhow::bail;
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
use rs_wordle_solver::WordleError;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use wordle::WORDS;
use wordle::time;
use wordle::word_bank;
use zarrs::array::Array;
use zarrs::array::ArrayBuilder;
use zarrs::array::ArraySubset;
use zarrs::array::FillValueMetadata;
use zarrs::array::data_type;
use zarrs::filesystem::FilesystemStore;
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
    store: ReadableWritableListableStorage,
    bank: &WordBank,
    target: BakeTarget<'_>,
) -> anyhow::Result<MaxScoreGuesser<MaxComboEliminationsScorer>> {
    let store_shape = [WORDS.len() as u64];
    let subset_all = ArraySubset::new_with_shape(vec![WORDS.len() as u64]);
    // could be other fs e.g. permission error but meh

    let score_key = format!("/{}/score", target.ident());
    let s_store = if let Ok(s) = Array::open(store.clone(), &score_key) {
        s
    } else {
        eprintln!("creating array {score_key}");

        let mut s = ArrayBuilder::new([WORDS.len() as u64], [1000], data_type::int64(), i64::MIN)
            .build(store.clone(), &score_key)?;
        s.set_dimension_names(Some(vec![Some("words".to_owned())]));
        s.store_metadata()?;

        s
    };

    let done_store = ArrayBuilder::new([1], [1], data_type::bool(), FillValueMetadata::Bool(false))
        .build(store.clone(), "/done")?;
    let done = done_store.retrieve_array_subset::<Vec<bool>>(&done_store.subset_all())?[0];

    let s_store = Arc::new(s_store);

    let words =
        Array::open(store.clone(), "/word")?.retrieve_array_subset::<Vec<String>>(&subset_all)?;
    let mut scores = s_store.retrieve_array_subset::<Vec<i64>>(&subset_all)?;

    let bank_todo = {
        if !done {
            words
                .iter()
                .zip(&scores)
                .filter_map(|(w, s)| (*s == i64::MIN).then_some(Arc::from(w.as_str())))
                .collect::<Vec<_>>()
        } else {
            Default::default()
        }
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
            match guesser.update(&GuessResult {
                guess: &guess,
                results: results.to_owned(),
            }) {
                Err(WordleError::InvalidResults) => {
                    eprintln!("this state is inconsistent; storing warning");
                    done_store.store_metadata()?;
                    done_store.store_array_subset(&done_store.subset_all(), vec![true])?;

                    let inconsistent_store = ArrayBuilder::new(
                        [],
                        <[u64; 0]>::default(),
                        data_type::bool(),
                        FillValueMetadata::Bool(false),
                    )
                    .build(store.clone(), "/inconsistent")?;
                    inconsistent_store.store_metadata()?;
                }
                Err(_) => bail!("io error updating; should not happen"),
                Ok(_) => {}
            }

            guesser
        }
    };

    let scores = if !bank_todo.is_empty() {
        time("bake missing words", || {
            let loaded_bake_progress = store_shape[0] - bank_todo.len() as u64;
            eprintln!(
                "baked {loaded_bake_progress}/{} for {}; baking {} more...",
                store_shape[0],
                target.ident(),
                bank_todo.len()
            );

            let bar = indicatif::ProgressBar::new(store_shape[0]);
            bar.set_position(loaded_bake_progress);
            bar.set_style(ProgressStyle::with_template(
                "{wide_bar} {pos}/{len} {per_sec} {elapsed_precise}/{eta_precise} remaining",
            )?);
            bar.enable_steady_tick(Duration::from_secs(1));

            let (score_tx, score_rx) = std::sync::mpsc::channel::<(usize, i64)>();

            let cron = std::thread::spawn({
                let s_store = s_store.clone();
                let bar = bar.clone();
                move || {
                    let subset_all = s_store.subset_all();
                    let mut last_store = SystemTime::now();
                    while let Ok((i, score)) = score_rx.recv() {
                        scores[i] = score;

                        let elapsed = last_store.elapsed();
                        if elapsed.is_err() || elapsed.unwrap() > Duration::from_secs(10) {
                            last_store = SystemTime::now();
                            match s_store.store_array_subset(&subset_all, &scores) {
                                Ok(_) => (),
                                Err(e) => bar.println(format!("err storing: {e}")),
                            }
                        }
                    }

                    match s_store.store_array_subset(&subset_all, &scores) {
                        Ok(_) => (),
                        Err(e) => bar.println(format!("err storing: {e}")),
                    }

                    scores
                }
            });

            bank_todo.par_iter().enumerate().for_each_init(
                || score_tx.clone(),
                |score_tx, (i, w)| {
                    let score = guesser.score_word(w);
                    score_tx.send((i, score)).unwrap();
                    bar.inc(1);
                },
            );

            drop(score_tx);
            bar.finish();

            eprintln!("baked scores done for state {}", target.ident(),);

            let scores = cron.join().ok().context("join cron")?;
            done_store.store_metadata()?;
            done_store.store_array_subset(&done_store.subset_all(), vec![true])?;

            anyhow::Ok(words.into_iter().map(Arc::from).zip(scores).collect())
        })?
    } else {
        eprintln!("{} was already baked", target.ident());
        words
            .into_iter()
            .map(Arc::from)
            .zip(scores)
            .collect::<HashMap<_, _>>()
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

    let store_path = data_path.join("baked.store");
    let store = Arc::new(FilesystemStore::new(&store_path)?) as ReadableWritableListableStorage;

    if (Array::open(store.clone(), "/word").is_err()) {
        eprintln!("creating store {}", store_path.display());

        zarrs::group::GroupBuilder::new()
            .build(store.clone(), "/")?
            .store_metadata()?;

        let mut w = ArrayBuilder::new([WORDS.len() as u64], [1000], data_type::string(), "")
            .build(store.clone(), "/word")?;
        w.set_dimension_names(Some(vec![Some("words".to_owned())]));
        w.store_metadata()?;
        w.store_array_subset(&w.subset_all(), &*WORDS)?;
    }

    let base_guesser = bake_for(store.clone(), &bank, BakeTarget::BaseState)?;
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
            store.clone(),
            &bank,
            BakeTarget::AfterResponse(&base_guesser, &result),
        )?;
    }

    Ok(())
}
