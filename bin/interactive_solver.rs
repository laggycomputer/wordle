use anyhow::Context as _;
use anyhow::bail;
use clap::Parser;
use pluralizer::pluralize;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::GuessResult;
use rs_wordle_solver::Guesser as _;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use std::collections::HashMap;
use std::io;
use std::io::Write as _;
use std::sync::Arc;
use wordle::time;
use zarrs::array::Array;
use zarrs::array::ArrayCreateError;
use zarrs::filesystem::FilesystemStore;
use zarrs::storage::ReadableWritableListableStorage;

#[derive(Debug, Parser)]
struct Options {
    #[arg(default_value = "1")]
    first_n: usize,
    #[arg(default_value = "10")]
    next_n: usize,
    #[arg(long, short)]
    simulate: Option<String>,
}

fn load_scores(
    store: &ReadableWritableListableStorage,
    buf: &mut HashMap<Arc<str>, i64>,
    words: &[Arc<str>],
    ident: &str,
) -> anyhow::Result<()> {
    let scores = Array::open(store.clone(), &format!("{ident}/score"))?;

    buf.clear();
    buf.extend(
        words.iter().cloned().zip(
            scores
                .retrieve_array_subset::<Vec<i64>>(&scores.subset_all())?
                .into_iter()
                .map(|s| match s {
                    i64::MIN => i64::MIN + 1,
                    o => o,
                }),
        ),
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut opts = Options::parse();

    let dirs = directories::ProjectDirs::from("com", "laggo", "wordle")
        .context("can't determine storage loc")?;

    let data_path = dirs.data_dir();

    let store_path = data_path.join("baked.zarr");
    let store = Arc::new(FilesystemStore::new(&store_path)?) as ReadableWritableListableStorage;
    let word_array = Array::open(store.clone(), "/word")?;
    let words = word_array
        .retrieve_array_subset::<Vec<String>>(&word_array.subset_all())?
        .into_iter()
        .map(Arc::from)
        .collect::<Vec<_>>();
    let bank = WordBank::from_iterator(words.iter().cloned())?;

    if let Some(ref mut s) = opts.simulate {
        s.make_ascii_lowercase();
        if !bank.contains(&Arc::from(&**s)) {
            bail!("invalid simulate target")
        }
    }

    let mut bake_path = "/0".to_owned();
    let mut scores_buf = HashMap::with_capacity(words.len());
    load_scores(&store, &mut scores_buf, &words, &bake_path)?;

    let mut guesser = MaxScoreGuesser::new(
        GuessFrom::AllUnguessedWords,
        bank.clone(),
        MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)?,
    )
    .with_scores(&scores_buf);

    let mut first_guess = true;
    let word_length = bank.word_length();
    let mut buf = String::with_capacity(word_length);

    let mut round = 0;

    while guesser.possible_words().len() > 1 {
        round += 1;

        let (timing_phase, mut how_many_total) = match first_guess {
            true => {
                first_guess = false;
                bake_path.clear();
                ("first guess", opts.first_n)
            }
            false => ("next guess", opts.next_n),
        };

        let mut guesses = time(timing_phase, || {
            guesser.select_top_n_guesses(how_many_total)
        });

        for (i, g) in guesses.iter().enumerate() {
            eprintln!("{}. {} ({})", i + 1, g.guess, g.score);
        }
        if let Some(more) = guesser.possible_words().len().checked_sub(guesses.len()) {
            eprintln!("and {more} more...");
        }
        io::stderr().flush()?;

        let best_guess = guesses[0].guess.clone();

        let guess = loop {
            eprint!("round {round}, enter your guess (word or index) or !more <x>: ");
            io::stderr().flush()?;
            buf.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            let trimmed = buf.trim();

            if let Some(cmd) = trimmed.strip_prefix('!') {
                match cmd.split_once(' ').unwrap_or((cmd, "")) {
                    ("more", how_many) if let Ok(how_many_more) = how_many.parse::<usize>() => {
                        how_many_total += how_many_more;
                        let more = guesser.select_top_n_guesses(how_many_total);
                        for (i, g) in more.iter().enumerate().skip(how_many_total - how_many_more) {
                            eprintln!("{}. {} ({})", i + 1, g.guess, g.score);
                        }
                        guesses = more;
                    }
                    _ => {}
                }
            }
            if let Ok(p) = trimmed.parse::<usize>()
                && p <= how_many_total
            {
                break guesses[p - 1].guess.clone();
            } else if trimmed.len() == word_length
                && trimmed.chars().all(|c| c.is_ascii_alphabetic())
                && bank.iter().any(|w| w.eq_ignore_ascii_case(trimmed))
            {
                break Arc::from(trimmed);
            } else if buf.is_empty() {
                // EOF
                return Ok(());
            }
        };

        let result = if let Some(ref s) = opts.simulate {
            let result = rs_wordle_solver::get_result_for_guess(s, &guess)
                .context("result for guess against simulated target")?;

            eprint!("given simulation target, the result of this guess is: ");
            result
                .results
                .iter()
                .map(|l| match l {
                    LetterResult::Correct => 'g',
                    LetterResult::PresentNotHere => 'y',
                    LetterResult::NotPresent => 'b',
                })
                .for_each(|l| eprint!("{l}"));
            eprintln!();

            result
        } else {
            let mut outcome = Vec::with_capacity(word_length);
            'outcome: loop {
                eprint!("enter the outcome, b/y/g: ");
                io::stderr().flush()?;

                buf.clear();
                outcome.clear();
                io::stdin().read_line(&mut buf).context("read stdin")?;

                if buf.is_empty() {
                    // EOF
                    return Ok(());
                }

                // Eb Eb Eb Eb Bb Db
                for c in buf.chars().take(word_length) {
                    outcome.push(match c {
                        'b' | 'B' => LetterResult::NotPresent,
                        'y' | 'Y' => LetterResult::PresentNotHere,
                        'g' | 'G' => LetterResult::Correct,
                        _ => {
                            eprintln!();
                            continue 'outcome;
                        }
                    });
                }

                break;
            }

            GuessResult {
                guess: &guess,
                results: outcome,
            }
        };

        guesser.update(&result).context("update state")?;

        if guesser.possible_words().len() > 1 {
            if guess == best_guess {
                bake_path.push('/');
                result
                    .results
                    .iter()
                    .map(|l| match l {
                        LetterResult::Correct => 'g',
                        LetterResult::PresentNotHere => 'y',
                        LetterResult::NotPresent => 'b',
                    })
                    .for_each(|l| bake_path.push(l));

                match load_scores(&store, &mut scores_buf, &words, &bake_path) {
                    Err(e)
                        if let Some(nf) = e.downcast_ref::<ArrayCreateError>()
                            && matches!(nf, ArrayCreateError::MissingMetadata) =>
                    {
                        eprintln!(
                            "WARNING: no longer using baked scores! proceed at your own risk..."
                        );
                    }
                    e @ Err(_) => return e,
                    Ok(_) => guesser = guesser.with_scores(&scores_buf),
                }
            } else {
                eprintln!("WARNING: no longer using baked scores! proceed at your own risk...");
            }
        }
    }

    round += 1;

    match &guesser.possible_words() {
        [one] => eprintln!(
            "the game is solved in {}: {one}",
            pluralize("round", round, true)
        ),
        [] => eprintln!("game is inconsistent :("),
        _ => unreachable!("we should be looping"),
    }

    Ok(())
}
