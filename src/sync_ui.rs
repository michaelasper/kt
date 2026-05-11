use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::stdout;
use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

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
            SyncUI::Pretty(_) => {}
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
    multi: MultiProgress,
    target_bar: ProgressBar,
    _rain_bars: Vec<ProgressBar>,
    chunks_bar: ProgressBar,
    total_files: usize,
    total_chunks: usize,
    tx: mpsc::Sender<()>,
    rain_handle: Option<std::thread::JoinHandle<()>>,
}

impl PrettySyncUI {
    fn new(total_files: usize) -> Self {
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());

        let target_bar = multi.add(ProgressBar::new_spinner());
        let target_style = ProgressStyle::with_template("  [{prefix:.cyan}] {msg:.bold}")
            .expect("Failed to parse target progress bar template")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈");
        target_bar.set_style(target_style);
        target_bar.set_prefix(" TARGET ");

        let mut rain_bars = Vec::new();
        for i in 0..3 {
            let bar = multi.add(ProgressBar::new_spinner());
            let rain_style = ProgressStyle::with_template("{msg}")
                .expect("Failed to parse rain progress bar template");
            bar.set_style(rain_style);
            if i == 0 {
                bar.set_message("           ↓  ↓  ↓  ↓  ↓");
            } else {
                bar.set_message("           0  1  0  1  0");
            }
            rain_bars.push(bar);
        }

        let spacer = multi.add(ProgressBar::new_spinner());
        spacer.set_style(
            ProgressStyle::with_template("").expect("Failed to parse spacer progress bar template"),
        );

        let chunks_bar = multi.add(ProgressBar::new(0));
        let chunks_style =
            ProgressStyle::with_template("  [{prefix:.cyan}] {wide_bar:.green/dim} ({pos}/{len})")
                .expect("Failed to parse chunks progress bar template")
                .progress_chars("▓▓░");
        chunks_bar.set_style(chunks_style);
        chunks_bar.set_prefix(" CHUNKS ");

        let (tx, rx) = mpsc::channel();
        let rain_bars_clone = rain_bars.clone();
        let handle = std::thread::Builder::new()
            .name("kt-rain".into())
            .spawn(move || rain_loop(rx, rain_bars_clone))
            .ok();

        Self {
            multi,
            target_bar,
            _rain_bars: rain_bars,
            chunks_bar,
            total_files,
            total_chunks: 0,
            tx,
            rain_handle: handle,
        }
    }

    fn start_file(&mut self, path: &str, file_index: usize) {
        self.target_bar.set_message(path.to_string());
        self.chunks_bar.set_length(0);
        self.chunks_bar.set_position(0);
        self.target_bar
            .set_prefix(format!(" TARGET {}/{}", file_index + 1, self.total_files));
        self.target_bar.tick();
    }

    fn finish_file(&mut self, _path: &str, chunks: usize) {
        self.total_chunks += chunks;
        self.chunks_bar.set_length(chunks as u64);
        self.chunks_bar.set_position(chunks as u64);
    }

    fn finish(mut self, total_files: usize, total_chunks: usize) {
        let _ = self.tx.send(());
        if let Some(handle) = self.rain_handle.take() {
            let _ = handle.join();
        }

        self.multi.clear().ok();

        let width = 51;
        let top = format_box_top(width);
        let bot = format_box_bot(width);
        let line1 = format_box_line(&format!("  {} SYNC COMPLETE", style("✓").green()), width);
        let detail = format!(
            "    {} files shredded into {} chunks",
            total_files, total_chunks
        );
        let line2 = format_box_line(&detail, width);

        println!("\n{top}\n{line1}\n{line2}\n{bot}\n");
    }
}

fn rain_loop(rx: mpsc::Receiver<()>, bars: Vec<ProgressBar>) {
    loop {
        if rx.try_recv().is_ok() {
            break;
        }

        bars[0].set_message("           ↓  ↓  ↓  ↓  ↓");
        bars[0].tick();

        for bar in bars.iter().skip(1) {
            let mut bits = String::with_capacity(20);
            bits.push_str("           ");
            for i in 0..5 {
                let bit = if fastrand::bool() { "1" } else { "0" };
                let colored = if fastrand::bool() {
                    style(bit).green().to_string()
                } else {
                    style(bit).cyan().to_string()
                };
                bits.push_str(&colored);
                if i < 4 {
                    bits.push_str("  ");
                }
            }
            bar.set_message(bits);
            bar.tick();
        }

        std::thread::sleep(Duration::from_millis(100));
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
