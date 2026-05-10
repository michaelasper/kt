use console::style;
use crossterm::{
    cursor::{MoveUp, RestorePosition, SavePosition},
    execute,
    style::{Color, Print, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{stdout, IsTerminal, Write};
use std::sync::mpsc;

const RAIN_CHARS: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];
const RAIN_WIDTH: usize = 40;
const RAIN_LINES: usize = 4;

enum Msg {
    NewFile,
    Stop,
}

pub enum SyncUI {
    Pretty(PrettySyncUI),
    Plain,
}

impl SyncUI {
    pub fn new(total_files: usize) -> Self {
        if stdout().is_terminal() {
            Self::Pretty(PrettySyncUI::new(total_files))
        } else {
            Self::Plain
        }
    }

    pub fn start_scan(&self, dir: &str) {
        match self {
            SyncUI::Pretty(ui) => ui.start_scan(dir),
            SyncUI::Plain => tracing::info!("Scanning {dir}..."),
        }
    }

    pub fn start_file(&mut self, path: &str, file_index: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.start_file(path, file_index),
            SyncUI::Plain => tracing::info!("Indexing {path}"),
        }
    }

    pub fn finish_file(&mut self, path: &str, chunks: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.finish_file(path, chunks),
            SyncUI::Plain => tracing::info!("Indexed {path} ({chunks} chunks)"),
        }
    }

    pub fn finish(self, total_files: usize, total_chunks: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.finish(total_files, total_chunks),
            SyncUI::Plain => {
                tracing::info!("Sync complete: {total_files} files, {total_chunks} chunks indexed")
            }
        }
    }
}

pub struct PrettySyncUI {
    total_files: usize,
    file_index: usize,
    total_chunks: usize,
    tx: mpsc::Sender<Msg>,
    rain_handle: Option<std::thread::JoinHandle<()>>,
    progress_bar: ProgressBar,
}

impl PrettySyncUI {
    fn new(total_files: usize) -> Self {
        let (tx, rx) = mpsc::channel();

        let handle = std::thread::Builder::new()
            .name("kt-rain".into())
            .spawn(move || rain_loop(rx))
            .ok();

        let pb = ProgressBar::hidden();
        pb.set_style(
            ProgressStyle::with_template(
                "  [{prefix:.cyan} {wide_bar:.green/dim} {pos}/{len}] {msg:.dim}",
            )
            .unwrap()
            .progress_chars("x=:"),
        );

        Self {
            total_files,
            file_index: 0,
            total_chunks: 0,
            tx,
            rain_handle: handle,
            progress_bar: pb,
        }
    }

    fn start_scan(&self, dir: &str) {
        let width = 51;
        let top = format_box_top(width);
        let bot = format_box_bot(width);
        let line = format_box_line(
            &format!("  {} {}", style("SCANNING").cyan().bold(), dir),
            width,
        );
        println!("\n{top}\n{line}\n{bot}\n");
    }

    fn start_file(&mut self, path: &str, file_index: usize) {
        self.file_index = file_index;

        let width = 51;
        let top = format_box_top(width);
        let bot = format_box_bot(width);
        let label = style("SHREDDING").cyan().bold();
        let content = format!("  {} {}", label, truncate_path(path, 36));
        let line = format_box_line(&content, width);

        println!("{top}");
        println!("{line}");
        println!("{bot}");
        println!();

        self.progress_bar.set_length(0);
        self.progress_bar.set_position(0);
        self.progress_bar.set_prefix("CHUNKS");
        self.progress_bar
            .set_message(format!("FILE {}/{}", file_index + 1, self.total_files));

        let _ = self.tx.send(Msg::NewFile);
    }

    fn finish_file(&mut self, _path: &str, chunks: usize) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(handle) = self.rain_handle.take() {
            let _ = handle.join();
        }

        self.progress_bar.finish_and_clear();
        println!(
            "  {} {} {} chunks extracted\n",
            style("x").green(),
            style(_path).dim(),
            style(chunks).green()
        );

        self.total_chunks += chunks;

        let (tx, rx) = mpsc::channel();
        self.tx = tx;
        let handle = std::thread::Builder::new()
            .name("kt-rain".into())
            .spawn(move || rain_loop(rx))
            .ok();
        self.rain_handle = handle;
    }

    fn finish(mut self, total_files: usize, total_chunks: usize) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(handle) = self.rain_handle.take() {
            let _ = handle.join();
        }
        self.progress_bar.finish_and_clear();

        let width = 51;
        let top = format_box_top(width);
        let bot = format_box_bot(width);
        let line1 = format_box_line(&format!("  {} SYNC COMPLETE", style("x").green()), width);
        let detail = format!(
            "    {} files shredded into {} chunks",
            total_files, total_chunks
        );
        let line2 = format_box_line(&detail, width);

        println!("{top}");
        println!("{line1}");
        println!("{line2}");
        println!("{bot}\n");
    }
}

fn rain_loop(rx: mpsc::Receiver<Msg>) {
    let mut rng = fastrand::Rng::new();
    let mut columns: Vec<Vec<char>> = vec![vec![' '; RAIN_LINES]; RAIN_WIDTH];
    let mut tick = 0u64;
    let mut active = false;

    loop {
        match rx.try_recv() {
            Ok(Msg::Stop) => {
                if active {
                    let mut stdout = stdout();
                    for _ in 0..RAIN_LINES {
                        let _ = execute!(stdout, MoveUp(1), Clear(ClearType::CurrentLine));
                    }
                }
                return;
            }
            Ok(Msg::NewFile) => {
                columns = vec![vec![' '; RAIN_LINES]; RAIN_WIDTH];
                tick = 0;
                active = true;
                for _ in 0..RAIN_LINES {
                    println!();
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if !active {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        let mut stdout = stdout();
        let _ = execute!(stdout, SavePosition);

        for (row, row_data) in columns.iter_mut().enumerate().take(RAIN_LINES) {
            let _ = execute!(
                stdout,
                MoveUp((RAIN_LINES - row) as u16),
                Clear(ClearType::CurrentLine)
            );

            for (col, cell) in row_data.iter_mut().enumerate() {
                let active_col = (tick + col as u64) % 7 < 4;
                if active_col || rng.bool() {
                    *cell = RAIN_CHARS[rng.usize(..RAIN_CHARS.len())];
                }

                if *cell != ' ' {
                    let bright = rng.bool();
                    let color = if bright {
                        Color::Green
                    } else {
                        Color::DarkCyan
                    };
                    let _ = execute!(stdout, SetForegroundColor(color), Print(*cell), Print(' '));
                } else {
                    print!("   ");
                }
            }
            println!();
        }

        let _ = execute!(stdout, RestorePosition);
        let _ = stdout.flush();

        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
}

fn format_box_top(width: usize) -> String {
    format!(
        "  {}{}{}",
        style("|=").cyan(),
        style("=".repeat(width)).cyan(),
        style("=|").cyan()
    )
}

fn format_box_bot(width: usize) -> String {
    format!(
        "  {}{}{}",
        style("|=").cyan(),
        style("=".repeat(width)).cyan(),
        style("=|").cyan()
    )
}

fn format_box_line(content: &str, width: usize) -> String {
    let stripped = console::strip_ansi_codes(content).len();
    let padding = width.saturating_sub(stripped);
    format!(
        "  {}{}{}{}",
        style("| ").cyan(),
        content,
        " ".repeat(padding),
        style(" |").cyan()
    )
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        let start = path.len() - max_len + 3;
        format!("...{}", &path[start..])
    }
}
