use anyhow::Context as _;
use anyhow::bail;
use clap::Parser;
use rs_wordle_solver::GuessResult;
use rs_wordle_solver::Guesser as _;
use rs_wordle_solver::LetterResult;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use std::io;
use std::io::Write as _;
use std::sync::Arc;
use wordle::time;
use wordle::word_bank;

#[derive(Debug, Parser)]
struct Options {
    #[arg(default_value = "1")]
    first_n: usize,
    #[arg(default_value = "10")]
    next_n: usize,
    #[arg(long, short)]
    simulate: Option<String>,
}

// TODO load bake

fn main() -> anyhow::Result<()> {
    let mut opts = Options::parse();

    let (bank, mut guesser) = time("init from baked", || {
        let bank = word_bank().context("word bank")?;

        anyhow::Ok((bank.clone(), {
            let decompressed = oxicode::compression::decompress(include_bytes!("../init.bin"))?;
            let (decoded, _) = oxicode::serde::decode_from_slice::<
                MaxScoreGuesser<MaxComboEliminationsScorer>,
                _,
            >(&decompressed, oxicode::config::standard())?;

            decoded
        }))
    })?;

    if let Some(ref mut s) = opts.simulate {
        s.make_ascii_lowercase();
        if !bank.contains(&Arc::from(&**s)) {
            bail!("invalid simulate target")
        }
    }

    let mut first_guess = true;
    let word_length = bank.word_length();
    let mut buf = String::with_capacity(word_length);

    while guesser.possible_words().len() > 1 {
        let mut guesses = time(
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

            print!("given simulation target, the result of this result is: ");
            result
                .results
                .iter()
                .map(|l| match l {
                    LetterResult::Correct => 'g',
                    LetterResult::PresentNotHere => 'y',
                    LetterResult::NotPresent => 'b',
                })
                .for_each(|l| print!("{l}"));
            println!();

            result
        } else {
            let mut outcome = Vec::with_capacity(word_length);
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
                for c in buf.chars().take(word_length) {
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

            GuessResult {
                guess: &guess,
                results: outcome,
            }
        };

        time("updated state", || guesser.update(&result)).context("update state")?;
    }

    match &guesser.possible_words() {
        [one] => println!("the game is solved: {one}"),
        [] => println!("game is inconsistent :("),
        _ => unreachable!("we should be looping"),
    }

    Ok(())
}
