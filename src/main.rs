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
    #[arg(short, action = clap::ArgAction::SetTrue)]
    thorough: bool,
}

enum GuesserDispatch {
    Cheap(MaxScoreGuesser<MaxEliminationsScorer>),
    Thorough(MaxScoreGuesser<MaxComboEliminationsScorer>),
}

impl Guesser for GuesserDispatch {
    fn update(&mut self, result: &GuessResult<'_>) -> Result<(), WordleError> {
        match self {
            GuesserDispatch::Cheap(guesser) => guesser.update(result),
            GuesserDispatch::Thorough(guesser) => guesser.update(result),
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
            GuesserDispatch::Cheap(guesser) => guesser.select_next_guess_from(from),
            GuesserDispatch::Thorough(guesser) => guesser.select_next_guess_from(from),
        }
    }

    fn possible_words(&self) -> &[Arc<str>] {
        match self {
            GuesserDispatch::Cheap(guesser) => guesser.possible_words(),
            GuesserDispatch::Thorough(guesser) => guesser.possible_words(),
        }
    }
}

impl GuesserDispatch {
    fn select_top_n_guesses(&mut self, n: usize) -> Vec<ScoredGuess> {
        match self {
            Self::Cheap(guesser) => guesser.select_top_n_guesses(n),
            Self::Thorough(guesser) => guesser.select_top_n_guesses(n),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Options::parse();

    let (bank, mut guesser) = time("init", || {
        let bank = WordBank::from_reader(Cursor::new(WORDS)).context("word bank")?;

        anyhow::Ok((
            bank.clone(),
            match opts.thorough {
                false => GuesserDispatch::Cheap(MaxScoreGuesser::new(
                    GuessFrom::PossibleWords,
                    bank.clone(),
                    MaxEliminationsScorer::new(bank.clone()),
                )),
                true => GuesserDispatch::Thorough(MaxScoreGuesser::new(
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

    while guesser.possible_words().len() > 1 {
        let guesses = time(
            match first_guess {
                true => "first guess",
                false => "next guess",
            },
            || {
                guesser.select_top_n_guesses(match first_guess {
                    true => {
                        first_guess = false;
                        opts.first_n
                    }
                    false => opts.next_n,
                })
            },
        );

        for (i, g) in guesses.iter().enumerate() {
            println!("{}. {} ({})", i + 1, g.guess, g.score);
        }
        io::stdout().flush()?;

        let mut how_many_total = opts.next_n;

        let guess = loop {
            print!("enter your guess (word or index) or !more <x>: ");
            io::stdout().flush()?;
            buf.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            let trimmed = buf.trim();

            if let Some(cmd) = trimmed.strip_prefix('!') {
                match cmd.split_once(' ').unwrap_or((cmd, "")) {
                    ("more", how_many) if let Ok(how_many_more) = how_many.parse::<usize>() => {
                        how_many_total += how_many_more;
                        let more = guesser.select_top_n_guesses(how_many_total);
                        for (i, g) in more.iter().enumerate().skip(how_many_total - how_many_more) {
                            println!("{}. {} ({})", i + 1, g.guess, g.score);
                        }
                    }
                    _ => {}
                }
            }
            if let Ok(p @ ..5) = trimmed.parse::<usize>() {
                break guesses[p - 1].guess.clone();
            } else if trimmed.len() == 5
                && trimmed.chars().all(|c| c.is_ascii_alphabetic())
                && bank.iter().any(|w| w.eq_ignore_ascii_case(trimmed))
            {
                break Arc::from(trimmed);
            } else if buf.is_empty() {
                // EOF
                return Ok(());
            }
        };

        let mut outcome = Vec::with_capacity(5);
        'outcome: loop {
            print!("enter the outcome, b/y/g: ");
            io::stdout().flush()?;

            buf.clear();
            outcome.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            if buf.is_empty() {
                // EOF
                return Ok(());
            }

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
            guesser.update(&GuessResult {
                guess: &guess,
                results: outcome,
            })
        })
        .context("update state")?;
    }

    match &guesser.possible_words() {
        [one] => println!("the game is solved: {one}"),
        [] => println!("game is inconsistent :("),
        _ => unreachable!("we should be looping"),
    }

    Ok(())
}
