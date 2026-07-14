use anyhow::Context as _;
use clap::Parser;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::GuessResult;
use rs_wordle_solver::Guesser as _;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::ScoredGuess;
use rs_wordle_solver::WordBank;
use rs_wordle_solver::scorers::MaxEliminationsScorer;
use std::io;
use std::io::Cursor;
use std::io::Write as _;
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
}

fn print_guesses<'g, I>(iter: I)
where
    I: IntoIterator<Item = &'g ScoredGuess>,
{
    for (i, g) in iter.into_iter().enumerate() {
        println!("{}. {} ({})", i + 1, g.guess, g.score);
    }
}

fn main() -> anyhow::Result<()> {
    let opts = Options::parse();

    let mut guesser = time("init", || {
        let bank = WordBank::from_reader(Cursor::new(WORDS)).context("word bank")?;
        let scorer = MaxEliminationsScorer::new(bank.clone());

        anyhow::Ok(MaxScoreGuesser::new(GuessFrom::PossibleWords, bank, scorer))
    })?;

    let mut first_guess = true;
    let mut buf = String::with_capacity(5);

    while guesser.possible_words().len() > 1 {
        let guesses = match first_guess {
            true => {
                first_guess = false;
                time("first guess", || guesser.select_top_n_guesses(opts.first_n))
            }
            false => time("next guess", || guesser.select_top_n_guesses(opts.next_n)),
        };
        print_guesses(&guesses);
        io::stdout().flush()?;

        let guess_i = loop {
            print!("enter your guess, the word or index: ");
            io::stdout().flush()?;
            buf.clear();
            io::stdin().read_line(&mut buf).context("read stdin")?;

            if let Ok(p) = buf.trim().parse::<usize>() {
                break p - 1;
            } else if let Some(by_word) = guesses
                .iter()
                .position(|g| g.guess.eq_ignore_ascii_case(buf.trim()))
            {
                break by_word;
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
            guesser.update(&GuessResult {
                guess: &guesses[guess_i].guess,
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
