use anyhow::Context as _;
use rs_wordle_solver::GuessFrom;
use rs_wordle_solver::MaxScoreGuesser;
use rs_wordle_solver::scorers::MaxComboEliminationsScorer;
use wordle::time;
use wordle::words;

fn main() -> anyhow::Result<()> {
    let bank = words().context("word bank")?;

    let guesser = time("init", || {
        let mut guesser = MaxScoreGuesser::new(
            GuessFrom::AllUnguessedWords,
            bank.clone(),
            #[expect(clippy::expect_used, reason = "i read the code man")]
            {
                MaxComboEliminationsScorer::new(bank.clone(), GuessFrom::AllUnguessedWords, 256)
                    .expect("appears to be infallible")
            },
        );

        guesser.get_or_compute_scores();

        guesser
    });

    let v = oxicode::serde::encode_to_vec(&guesser, oxicode::config::standard())?;
    let compressed =
        oxicode::compression::compress(&v, oxicode::compression::Compression::ZstdLevel(22))?;
    std::fs::write("init.bin", &compressed)?;
    println!("{} compressed", compressed.len());

    Ok(())
}
