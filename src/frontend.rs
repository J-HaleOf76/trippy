use crate::backend::Hop;
use crate::dns::DnsResolver;
use crate::{IcmpTracerConfig, Trace};
use chrono::SecondsFormat;
use crossterm::{
    event::{self, DisableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use itertools::Itertools;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tui::layout::{Alignment, Direction, Rect};
use tui::text::{Span, Spans};
use tui::widgets::{BarChart, BorderType, Paragraph, Sparkline, TableState};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame, Terminal,
};

const TABLE_HEADER: [&str; 11] = [
    "#", "Host", "Loss%", "Snt", "Recv", "Last", "Avg", "Best", "Wrst", "StDev", "Sts",
];

const TABLE_WIDTH: [Constraint; 11] = [
    Constraint::Percentage(3),
    Constraint::Percentage(42),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
];

struct TuiApp {
    table_state: TableState,
    trace: Trace,
    resolver: DnsResolver,
    target_hostname: String,
    target_addr: IpAddr,
    tracer_config: IcmpTracerConfig,
}

impl TuiApp {
    fn new(target_hostname: String, target_addr: IpAddr, tracer_config: IcmpTracerConfig) -> Self {
        Self {
            table_state: TableState::default(),
            trace: Trace::default(),
            resolver: DnsResolver::default(),
            target_hostname,
            target_addr,
            tracer_config,
        }
    }
    pub fn next(&mut self) {
        let max_index = 0.max(usize::from(self.trace.highest_ttl) - 1);
        let i = match self.table_state.selected() {
            Some(i) => {
                if i < max_index {
                    i + 1
                } else {
                    i
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i > 0 {
                    i - 1
                } else {
                    i
                }
            }
            None => 0.max(usize::from(self.trace.highest_ttl) - 1),
        };
        self.table_state.select(Some(i));
    }

    pub fn clear(&mut self) {
        self.table_state.select(None);
    }
}

/// Run the frontend TUI.
pub fn run_frontend(
    target_hostname: String,
    target_addr: IpAddr,
    data: &Arc<RwLock<Trace>>,
    config: IcmpTracerConfig,
    preserve_screen: bool,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let res = run_app(&mut terminal, data, target_hostname, target_addr, config);
    disable_raw_mode()?;
    if !preserve_screen {
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    }
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    if let Err(err) = res {
        println!("{:?}", err);
    }
    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    trace: &Arc<RwLock<Trace>>,
    target_hostname: String,
    target_addr: IpAddr,
    config: IcmpTracerConfig,
) -> io::Result<()> {
    let mut app = TuiApp::new(target_hostname, target_addr, config);
    loop {
        app.trace = trace.read().clone();
        terminal.draw(|f| render_all(f, &mut app))?;
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    KeyCode::Esc => app.clear(),
                    _ => {}
                }
            }
        }
    }
}

/// Render the TUI.
///
/// The layout of the TUI is as follows:
///
///  ____________________________________
/// |               Header               |
///  ------------------------------------
/// |                                    |
/// |                                    |
/// |                                    |
/// |               Hops                 |
/// |                                    |
/// |                                    |
/// |                                    |
///  ------------------------------------
/// |     History     |    Frequency     |
/// |                 |                  |
///  ------------------------------------
///
/// Header - the title, configuration, destination, clock and keyboard controls
/// Hops - a table where each row represents a single hop (time-to-live) in the trace
/// History - a graph of historic round-trip ping samples for the target host
/// Frequency - a histogram of sample frequencies by round-trip time for the target host
///
/// On startup a splash screen is shown in place of the hops table, until the completion of the first round.
fn render_all<B: Backend>(f: &mut Frame<'_, B>, app: &mut TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage(15),
                Constraint::Percentage(65),
                Constraint::Percentage(20),
            ]
            .as_ref(),
        )
        .split(f.size());
    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)].as_ref())
        .split(chunks[2]);
    render_header(f, app, chunks[0]);
    if app.trace.highest_ttl > 0 {
        render_table(f, app, chunks[1]);
    } else {
        render_splash(f, chunks[1]);
    }
    render_history(f, app, bottom_chunks[0]);
    render_ping_frequency(f, app, bottom_chunks[1]);
}

/// Render the title, config, target, clock and keyboard controls.
fn render_header<B: Backend>(f: &mut Frame<'_, B>, app: &mut TuiApp, rect: Rect) {
    let header_block = Block::default()
        .title(format!(" Trippy v{} ", clap::crate_version!()))
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default());
    let now = chrono::Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let clock_span = Spans::from(Span::raw(now));
    let help_span = Spans::from(vec![
        Span::styled("h", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("elp "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("uit"),
    ]);
    let right_spans = vec![clock_span, help_span];
    let right = Paragraph::new(right_spans)
        .style(Style::default())
        .block(header_block.clone())
        .alignment(Alignment::Right);
    f.render_widget(right, rect);
    let left_spans = vec![
        Spans::from(vec![
            Span::styled("Target: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{} ({})", app.target_hostname, app.target_addr)),
        ]),
        Spans::from(vec![
            Span::styled("Config: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "mode=icmp interval={} grace={} start-ttl={} max-ttl={}",
                humantime::format_duration(app.tracer_config.min_round_duration),
                humantime::format_duration(app.tracer_config.grace_duration),
                app.tracer_config.first_ttl.0,
                app.tracer_config.max_ttl.0
            )),
        ]),
    ];

    let left = Paragraph::new(left_spans)
        .style(Style::default())
        .block(header_block)
        .alignment(Alignment::Left);
    f.render_widget(left, rect);
}

/// Render the splash screen.
///
/// This is shown on startup whilst we await the first round of data to be available.
fn render_splash<B: Backend>(f: &mut Frame<'_, B>, rect: Rect) {
    let chunks = Layout::default()
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)].as_ref())
        .split(rect);
    let block = Block::default()
        .title("Hops")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default());
    let splash = vec![
        r#" _____    _                "#,
        r#"|_   _| _(_)_ __ _ __ _  _ "#,
        r#"  | || '_| | '_ \ '_ \ || |"#,
        r#"  |_||_| |_| .__/ .__/\_, |"#,
        r#"           |_|  |_|   |__/ "#,
        "",
        "Starting...",
    ];
    let spans: Vec<_> = splash
        .into_iter()
        .map(|line| Spans::from(Span::styled(line, Style::default())))
        .collect();
    let paragraph = Paragraph::new(spans).alignment(Alignment::Center);
    f.render_widget(block, rect);
    f.render_widget(paragraph, chunks[1]);
}

/// Render the table of data about the hops.
///
/// For each hop, we show:
///
/// - The time-to-live (indexed from 1) at this hop (`#`)
/// - The host(s) reported at this hop (`Host`)
/// - The packet loss % for all probes at this hop (`Loss%`)
/// - The number of requests sent for all probes at this hop (`Snt`)
/// - The number of replies received for all probes at this hop (`Recv`)
/// - The round-trip time of the most recent probe at this hop (`Last`)
/// - The average round-trip time for all probes at this hop (`Avg`)
/// - The best round-trip time for all probes at this hop (`Best`)
/// - The worst round-trip time for all probes at this hop (`Wrst`)
/// - The standard deviation round-trip time for all probes at this hop (`StDev`)
/// - The status of this hop (`Sts`)
fn render_table<B: Backend>(f: &mut Frame<'_, B>, app: &mut TuiApp, rect: Rect) {
    let header = render_table_header();
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let rows = app
        .trace
        .hops
        .iter()
        .take(app.trace.highest_ttl as usize)
        .enumerate()
        .map(|(i, hop)| render_table_row(hop, &mut app.resolver, i, app.trace.highest_ttl));
    let table = Table::new(rows)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Hops"),
        )
        .highlight_style(selected_style)
        .widths(&TABLE_WIDTH);
    f.render_stateful_widget(table, rect, &mut app.table_state);
}

/// Render the table header.
fn render_table_header() -> Row<'static> {
    let header_cells = TABLE_HEADER
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Black)));
    Row::new(header_cells)
        .style(Style::default().bg(Color::White))
        .height(1)
        .bottom_margin(0)
}

/// Render a single row in the table of hops.
fn render_table_row(hop: &Hop, dns: &mut DnsResolver, i: usize, highest_ttl: u8) -> Row<'static> {
    let ttl_cell = render_ttl_cell(hop);
    let hostname_cell = render_hostname_cell(hop, dns);
    let loss_pct_cell = render_loss_pct_cell(hop);
    let total_sent_cell = render_total_sent_cell(hop);
    let total_recv_cell = render_total_recv_cell(hop);
    let last_cell = render_last_cell(hop);
    let avg_cell = render_avg_cell(hop);
    let best_cell = render_best_cell(hop);
    let worst_cell = render_worst_cell(hop);
    let stddev_cell = render_stddev_cell(hop);
    let status_cell = render_status_cell(hop, i, highest_ttl);
    let cells = [
        ttl_cell,
        hostname_cell,
        loss_pct_cell,
        total_sent_cell,
        total_recv_cell,
        last_cell,
        avg_cell,
        best_cell,
        worst_cell,
        stddev_cell,
        status_cell,
    ];
    let row_height = hop.addrs.len().max(1) as u16;
    Row::new(cells).height(row_height).bottom_margin(0)
}

fn render_ttl_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.ttl))
}

fn render_loss_pct_cell(hop: &Hop) -> Cell<'static> {
    let lost = hop.total_sent - hop.total_recv;
    let loss_pct = lost as f64 / hop.total_sent as f64 * 100f64;
    Cell::from(format!("{:.1}%", loss_pct))
}

fn render_total_sent_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.total_sent))
}

fn render_total_recv_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.total_recv))
}

fn render_avg_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(if hop.total_recv > 0 {
        format!(
            "{:.1}",
            hop.total_time.as_secs_f64() / hop.total_recv as f64
        )
    } else {
        String::default()
    })
}

fn render_hostname_cell(hop: &Hop, dns: &mut DnsResolver) -> Cell<'static> {
    Cell::from(if hop.total_recv > 0 {
        hop.addrs
            .iter()
            .map(|addr| format!("{} ({})", dns.reverse_lookup(*addr), addr))
            .join("\n")
    } else {
        String::from("No response")
    })
}

fn render_last_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.last
            .map(|dur| dur.as_millis().to_string())
            .unwrap_or_default(),
    )
}

fn render_best_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.best
            .map(|dur| dur.as_millis().to_string())
            .unwrap_or_default(),
    )
}

fn render_worst_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.worst
            .map(|dur| dur.as_millis().to_string())
            .unwrap_or_default(),
    )
}

fn render_stddev_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(if hop.total_recv > 1 {
        format!("{:.1}", (hop.m2 / (hop.total_recv - 1) as f64).sqrt())
    } else {
        String::default()
    })
}

fn render_status_cell(hop: &Hop, i: usize, highest_ttl: u8) -> Cell<'static> {
    let lost = hop.total_sent - hop.total_recv;
    Cell::from(match (lost, usize::from(highest_ttl) == i + 1) {
        (lost, target) if target && lost == hop.total_sent => "🔴",
        (lost, target) if target && lost > 0 => "🟡",
        (lost, target) if !target && lost == hop.total_sent => "🟤",
        (lost, target) if !target && lost > 0 => "🔵",
        _ => "🟢",
    })
}

/// Render the ping history for the final hop which is typically the target.
fn render_history<B: Backend>(f: &mut Frame<'_, B>, app: &mut TuiApp, rect: Rect) {
    let target_hop = app
        .table_state
        .selected()
        .map_or_else(|| app.trace.target_hop(), |s| &app.trace.hops[s]);
    let max_samples = target_hop.samples.len().min(rect.width as usize);
    let data = &target_hop.samples[0..max_samples]
        .iter()
        .map(|s| s.as_millis() as u64)
        .collect::<Vec<_>>();
    let history = Sparkline::default()
        .block(
            Block::default()
                .title(format!("Samples #{}", target_hop.ttl))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .data(data)
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(history, rect);
}

/// Render a histogram of ping frequencies.
fn render_ping_frequency<B: Backend>(f: &mut Frame<'_, B>, app: &mut TuiApp, rect: Rect) {
    let target_hop = app
        .table_state
        .selected()
        .map_or_else(|| app.trace.target_hop(), |s| &app.trace.hops[s]);
    let freq_data = sample_frequency(&target_hop.samples);
    let freq_data_ref: Vec<_> = freq_data.iter().map(|(b, c)| (b.as_str(), *c)).collect();
    let barchart = BarChart::default()
        .block(
            Block::default()
                .title(format!("Frequency #{}", target_hop.ttl))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .data(freq_data_ref.as_slice())
        .bar_width(4)
        .bar_gap(1)
        .bar_style(Style::default().fg(Color::Green))
        .value_style(
            Style::default()
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(barchart, rect);
}

/// Return the frequency % grouped by sample duration.
fn sample_frequency(samples: &[Duration]) -> Vec<(String, u64)> {
    let sample_count = samples.len();
    let mut count_by_duration: BTreeMap<u128, u64> = BTreeMap::new();
    for sample in samples {
        if sample.as_millis() > 0 {
            *count_by_duration.entry(sample.as_millis()).or_default() += 1;
        }
    }
    count_by_duration
        .iter()
        .map(|(ping, count)| {
            let ping = format!("{}", ping);
            let freq_pct = ((*count as f64 / sample_count as f64) * 100_f64) as u64;
            (ping, freq_pct)
        })
        .collect()
}