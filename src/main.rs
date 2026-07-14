use anyhow::Context as _;
use clap::Parser;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::GuessResult;
use rs_wordle_solver::Guesser;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::ScoredGuess;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::WordleError;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use rs_wordle_solver::scorers::MaxEliminationsScorer;
use std::io;
use std::io::Cursor;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

fn time<F, R>(phase: &str, func: F) -> R
where
    F: FnOnce() -> R,
{
    let start = Instant::now();
    let r = func();
    let duration = start.elapsed();
    println!("{phase} in {}", humantime::format_duration(duration));
    r
}

const WORDS: &[u8; 89130] = include_bytes!("../bank.txt");

#[derive(Debug, Parser)]
struct Options {
    #[arg(default_value = "1")]
    first_n: usize,
    #[arg(default_value = "10")]
    next_n: usize,
    #[arg(default_value = "false")]
    thorough: bool,
}

fn print_guesses<'g, I>(iter: I)
where
    I: IntoIterator<Item = &'g ScoredGuess>,
{
    for (i, g) in iter.into_iter().enumerate() {
        println!("{}. {} ({})", i + 1, g.guess, g.score);
    }
}

enum Scorer {
    Cheap(MaxScoreGuesser<MaxEliminationsScorer>),
    Thorough(MaxScoreGuesser<MaxComboEliminationsScorer>),
}

impl Guesser for Scorer {
    fn update(&mut self, result: &GuessResult<'_>) -> Result<(), WordleError> {
        match self {
            Scorer::Cheap(guesser) => guesser.update(result),
            Scorer::Thorough(guesser) => guesser.update(result),
        }
    }

    fn select_next_guess(&mut self) -> Option<Arc<str>> {
        match self {
            Self::Cheap(guesser) => guesser.select_next_guess(),
            Self::Thorough(guesser) => guesser.select_next_guess(),
        }
    }

    fn select_next_guess_from(&mut self, from: GuessFrom) -> Option<Arc<str>> {
        match self {
            Scorer::Cheap(guesser) => guesser.select_next_guess_from(from),
            Scorer::Thorough(guesser) => guesser.select_next_guess_from(from),
        }
    }

    fn possible_words(&self) -> &[Arc<str>] {
        match self {
            Scorer::Cheap(guesser) => guesser.possible_words(),
            Scorer::Thorough(guesser) => guesser.possible_words(),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Options::parse();

    let (bank, mut engine) = time("init", || {
        let bank = WordBank::from_reader(Cursor::new(WORDS)).context("word bank")?;

        anyhow::Ok((
            bank.clone(),
            match opts.thorough {
                false => Scorer::Cheap(MaxScoreGuesser::new(
                    GuessFrom::PossibleWords,
                    bank.clone(),
                    MaxEliminationsScorer::new(bank.clone()),
                )),
                true => Scorer::Thorough(MaxScoreGuesser::new(
                    GuessFrom::PossibleWords,
                    bank.clone(),
                    #[expect(clippy::expect_used, reason = "i read the code man")]
                    {
                        MaxComboEliminationsScorer::new(
                            bank.clone(),
                            GuessFrom::AllUnguessedWords,
                            256,
                        )
                        .expect("appears to be infallible")
                    },
                )),
            },
        ))
    })?;

    let mut first_guess = true;
    let mut buf = String::with_capacity(5);

    while engine.possible_words().len() > 1 {
        let guesses = time(
            match first_guess {
                true => {
                    first_guess = false;
                    "first guess"
                }
                false => "next guess",
            },
            || match engine {
                Scorer::Cheap(ref mut e) => e.select_top_n_guesses(opts.first_n),
                Scorer::Thorough(ref mut e) => e.select_top_n_guesses(opts.first_n),
            },
        );
        print_guesses(&guesses);
        io::stdout().flush()?;

        let guess = loop {
            print!("enter your guess, the word or index: ");
            io::stdout().flush()?;
            buf.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            let trimmed = buf.trim();

            if let Ok(p @ ..5) = trimmed.parse::<usize>() {
                break guesses[p - 1].guess.clone();
            } else if trimmed.len() == 5
                && trimmed.chars().all(|c| c.is_ascii_alphabetic())
                && bank.iter().any(|w| w.eq_ignore_ascii_case(trimmed))
            {
                break Arc::from(trimmed);
            }
        };

        let mut outcome = Vec::with_capacity(5);
        'outcome: loop {
            print!("enter the outcome, b/y/g: ");
            io::stdout().flush()?;

            buf.clear();
            outcome.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            // Eb Eb Eb Eb Bb Db
            for c in buf.chars().take(5) {
                outcome.push(match c {
                    'b' | 'B' => LetterResult::NotPresent,
                    'y' | 'Y' => LetterResult::PresentNotHere,
                    'g' | 'G' => LetterResult::Correct,
                    _ => {
                        println!();
                        continue 'outcome;
                    }
                });
            }

            break;
        }

        time("updated state", || {
            engine.update(&GuessResult {
                guess: &guess,
                results: outcome,
            })
        })
        .context("update state")?;
    }

    match &engine.possible_words() {
        [one] => println!("the game is solved: {one}"),
        [] => println!("game is inconsistent :("),
        _ => unreachable!("we should be looping"),
    }

    Ok(())
}
