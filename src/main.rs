mod api;
mod app;
mod config;
mod doc;
mod download;
mod input;
mod ui;

use std::env;
use std::process;
use std::time::Duration;

use anyhow::{bail, Result};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use api::{EdClient, Me};
use app::App;
use config::{Options, Region};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let options = match config::parse_args(&args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("{}\nerror: {e}", config::usage());
            process::exit(2);
        }
    };
    if options.show_help {
        print!("{}", config::usage());
        return;
    }
    if options.show_version {
        println!("edstem-tui {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let Some(token) = config::load_token() else {
        eprintln!(
            "No Ed API token found.\n\n\
             1. Create a token at {} (or {})\n\
             2. Save it:\n     mkdir -p {dir} && read -rs t && printf '%s\\n' \"$t\" > {dir}/token && chmod 600 {dir}/token\n\
             \x20  or set EDSTEM_TOKEN.",
            Region::Us.token_url(),
            Region::Au.token_url(),
            dir = config::tilde(&config::config_dir()),
        );
        process::exit(1);
    };

    eprintln!("Connecting to Ed…");
    let (client, me, web_base) = match connect(&token, &options) {
        Ok(connected) => connected,
        Err(e) => {
            eprintln!("error: {e:#}");
            process::exit(1);
        }
    };

    let download_root = options
        .download_dir
        .clone()
        .unwrap_or_else(config::default_download_dir);
    let mut app = match App::new(client, me, web_base, download_root) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error: {e:#}");
            process::exit(1);
        }
    };

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

/// Picks the API base: EDSTEM_BASE_URL, then --region, then the remembered
/// region, then whichever region accepts the token (which is remembered).
fn connect(token: &str, options: &Options) -> Result<(EdClient, Me, Option<String>)> {
    if let Ok(base) = env::var("EDSTEM_BASE_URL") {
        if !base.trim().is_empty() {
            let client = EdClient::new(base.trim(), token)?;
            let me = client.user()?;
            return Ok((client, me, None));
        }
    }

    let (regions, auto) = match options.region {
        Some(region) => (vec![region], false),
        None => {
            let mut regions: Vec<Region> = config::saved_region().into_iter().collect();
            for region in Region::ALL {
                if !regions.contains(&region) {
                    regions.push(region);
                }
            }
            (regions, true)
        }
    };

    let mut errors = Vec::new();
    for region in regions {
        let client = EdClient::new(region.api_base(), token)?;
        match client.user() {
            Ok(me) => {
                if auto {
                    config::save_region(region);
                }
                return Ok((client, me, Some(region.web_base())));
            }
            Err(e) => errors.push(format!("{}: {e:#}", region.code())),
        }
    }
    bail!("could not sign in to Ed\n  {}", errors.join("\n  "))
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while app.running {
        terminal.draw(|f| ui::draw(f, app))?;
        while let Ok(msg) = app.rx.try_recv() {
            app.on_msg(msg);
        }
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    input::handle_key(app, key);
                }
            }
        }
    }
    Ok(())
}
