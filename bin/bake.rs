use anyhow::Context as _;
use core::fmt::Formatter;
use core::fmt::Write as _;
use std::io::Write as _;
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
use rs_wordle_solver::details::WordRestrictions;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use rs_wordle_solver::scorers::WordScorer;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
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
    AfterResponse(&'o MaxComboEliminationsScorer, &'o [LetterResult]),
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

struct TakeableScorer<S: WordScorer>(Arc<RwLock<S>>);

impl<S> Clone for TakeableScorer<S>
where
    S: WordScorer,
{
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<S> TakeableScorer<S>
where
    S: WordScorer,
{
    fn new(scorer: S) -> Self {
        Self(Arc::new(RwLock::new(scorer)))
    }
}

impl<S: WordScorer> WordScorer for TakeableScorer<S> {
    fn update(
        &mut self,
        latest_guess: &str,
        restrictions: &WordRestrictions,
        possible_words: &[Arc<str>],
    ) -> Result<(), WordleError> {
        self.0
            .write()
            .unwrap()
            .update(latest_guess, restrictions, possible_words)
    }

    fn score_word(&self, word: &Arc<str>) -> i64 {
        self.0.read().unwrap().score_word(word)
    }
}

fn bake_for(
    dirs: &directories::ProjectDirs,
    bank: &WordBank,
    target: BakeTarget<'_>,
) -> anyhow::Result<(
    MaxScoreGuesser<TakeableScorer<MaxComboEliminationsScorer>>,
    TakeableScorer<MaxComboEliminationsScorer>,
)> {
    let data_path = dirs.data_dir();

    eprintln!("baking {}", target.ident());

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

    let words = w_store.retrieve_array_subset::<Vec<String>>(&subset_all)?;
    let scores = s_store.retrieve_array_subset::<Vec<i64>>(&subset_all)?;

    let bank_todo = {
        words
            .iter()
            .zip(&scores)
            .filter_map(|(w, s)| (*s == i64::MIN).then_some(Arc::from(w.as_str())))
            .collect::<Vec<_>>()
    };

    let (mut guesser, scorer) = match target {
        BakeTarget::BaseState => {
            #[expect(clippy::expect_used, reason = "i read the code man")]
            let scorer = TakeableScorer::new(
                MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)
                    .expect("appears to be infallible"),
            );
            (
                MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer.clone()),
                scorer,
            )
        }
        BakeTarget::AfterResponse(scorer, results) => {
            let scorer = TakeableScorer::new(scorer.clone());
            let mut guesser =
                MaxScoreGuesser::new(GuessFrom::AllUnguessedWords, bank.clone(), scorer.clone());
            let guess = guesser.select_next_guess().context("best next guess")?;
            guesser.update(&GuessResult {
                guess: &guess,
                results: results.to_owned(),
            })?;

            (guesser, scorer)
        }
    };

    guesser = guesser.with_scores(&if !bank_todo.is_empty() {
        time("bake missing words", || {
            let scores = Mutex::new(scores);

            let loaded_bake_progress = store_shape[0] - bank_todo.len() as u64;
            eprintln!(
                "baked {loaded_bake_progress}/{}; baking {} more...",
                store_shape[0],
                bank_todo.len()
            );
            let bar = indicatif::ProgressBar::new(store_shape[0]);
            bar.set_position(loaded_bake_progress);
            bar.force_draw();
            let _ = std::io::stderr().flush();

            bank_todo.par_iter().enumerate().for_each(|(i, w)| {
                let score = scorer.score_word(w);
                scores.lock().unwrap()[i] = score;
                let _ = s_store
                    .store_array_subset(&ArraySubset::new_with_shape(vec![i as u64]), &[score]);
                bar.inc(1);
            });

            bar.finish();

            eprintln!(
                "baked guesser is finished in state {} at {}",
                target.ident(),
                store_path.display(),
            );

            let scores = scores.into_inner()?;
            s_store.store_array_subset(&subset_all, &scores)?;

            anyhow::Ok(words.into_iter().map(Arc::from).zip(scores).collect())
        })?
    } else {
        words.into_iter().map(Arc::from).zip(scores).collect()
    });

    Ok((guesser, scorer))
}

fn main() -> anyhow::Result<()> {
    let bank = word_bank().context("word bank")?;

    let dirs = directories::ProjectDirs::from("com", "laggo", "wordle")
        .context("can't determine storage loc")?;

    let data_path = dirs.data_dir();
    std::fs::create_dir_all(data_path)
        .with_context(|| format!("can't create {}", data_path.display()))?;

    let (_base_guesser, base_scorer) = bake_for(&dirs, &bank, BakeTarget::BaseState)?;
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
            BakeTarget::AfterResponse(&base_scorer.0.read().unwrap(), &result),
        )?;
    }

    Ok(())
}
