//! Native layout of CommonMark and GitHub-style message Markdown.
use crate::paint::{Canvas, Rect, TextStyle, Type};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};
use std::sync::Arc;

const INK: u32 = 0xff1c3029;
const MUTED: u32 = 0xff50665b;
const LINK: u32 = 0xff13634e;
const LINE: u32 = 0xffcfded4;
const TINT: u32 = 0xffeaf2e9;
const CODE: u32 = 0xfff1f4ee;
const BODY: i32 = 16;
const MIN_COLUMN: i32 = 56;

enum Node {
    Group(Tag<'static>, Vec<Node>),
    Leaf(Event<'static>),
}

fn parse(source: &str) -> Option<Vec<Node>> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut root = Vec::new();
    let mut stack: Vec<(Tag<'static>, Vec<Node>)> = Vec::new();
    for (index, event) in Parser::new_ext(source, options).enumerate() {
        // Messages are untrusted. Bound recursion and rendered expansion,
        // including implicit cells in sparse tables.
        if index >= 8192 || stack.len() >= 48 {
            return None;
        }
        let node = match event {
            Event::Start(tag) => {
                stack.push((tag.into_static(), Vec::new()));
                continue;
            }
            Event::End(_) => {
                let Some((tag, children)) = stack.pop() else {
                    continue;
                };
                Node::Group(tag, children)
            }
            leaf => Node::Leaf(leaf.into_static()),
        };
        if let Some((_, children)) = stack.last_mut() {
            children.push(node);
        } else {
            root.push(node);
        }
    }
    Some(root)
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Style {
    text: TextStyle,
    strike: bool,
    link: Option<Arc<str>>,
}

#[derive(Clone)]
struct Span {
    text: String,
    style: Style,
}

fn add_span(spans: &mut Vec<Span>, text: &str, style: &Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut().filter(|last| last.style == *style) {
        last.text.push_str(text);
    } else {
        spans.push(Span {
            text: text.into(),
            style: style.clone(),
        });
    }
}

fn web_link(url: &str) -> Option<Arc<str>> {
    let (_, rest) = url.split_once("://")?;
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
        || rest.split(['/', '?', '#']).next()?.is_empty()
    {
        return None;
    }
    Some(Arc::from(url))
}

fn inline(nodes: &[Node], spans: &mut Vec<Span>, style: &Style) {
    for node in nodes {
        match node {
            Node::Group(tag, children) => {
                let mut child_style = style.clone();
                match tag {
                    Tag::Emphasis => child_style.text.italic = true,
                    Tag::Strong => child_style.text.bold = true,
                    Tag::Strikethrough => child_style.strike = true,
                    Tag::Link { dest_url, .. } => child_style.link = web_link(dest_url),
                    Tag::Image { .. } => {
                        add_span(spans, "[Image: ", style);
                        inline(children, spans, style);
                        add_span(spans, "]", style);
                        continue;
                    }
                    _ => {}
                }
                inline(children, spans, &child_style);
            }
            Node::Leaf(event) => match event {
                Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                    add_span(spans, text, style)
                }
                Event::Code(text) => {
                    let mut code = style.clone();
                    code.text.mono = true;
                    add_span(spans, text, &code);
                }
                Event::SoftBreak | Event::HardBreak => add_span(spans, "\n", style),
                Event::TaskListMarker(checked) => {
                    add_span(spans, if *checked { "[x] " } else { "[ ] " }, style)
                }
                _ => {}
            },
        }
    }
}

#[derive(Default)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    commands: Vec<Draw>,
}

enum Draw {
    Fill(Rect, u32),
    Text(Rect, i32, Span, u32),
}

impl Layout {
    pub fn message(body: &str, width: i32, font: &Type) -> Self {
        let mut builder = Builder {
            font,
            layout: Self::default(),
        };
        if let Some(nodes) = parse(body) {
            builder.blocks(&nodes, 0, width, 0);
        } else {
            let mut spans = vec![Span {
                text: body.into(),
                style: Style::default(),
            }];
            builder.paragraph(&mut spans, 0, width, 0);
        }
        builder.layout
    }

    pub fn draw(
        &self,
        canvas: &mut Canvas,
        font: &mut Type,
        origin: (i32, i32),
        mut link_hit: impl FnMut(Rect, Arc<str>),
    ) {
        for command in &self.commands {
            let shift = |r: Rect| Rect::new(origin.0 + r.x, origin.1 + r.y, r.w, r.h);
            match command {
                Draw::Fill(rect, color) => canvas.fill(shift(*rect), *color),
                Draw::Text(rect, size, span, color) => {
                    let rect = shift(*rect);
                    let clip = canvas.clip;
                    if rect.y >= clip.y + clip.h || rect.y + rect.h <= clip.y {
                        continue;
                    }
                    if span.style.text.mono {
                        canvas.round(
                            Rect::new(rect.x - 1, rect.y, rect.w + 2, rect.h - 2),
                            3,
                            CODE,
                        );
                    }
                    canvas.styled_text(
                        font,
                        (rect.x, rect.y),
                        *size,
                        &span.text,
                        *color,
                        span.style.text,
                    );
                    if span.style.strike {
                        canvas.fill(Rect::new(rect.x, rect.y + size / 2, rect.w, 1), *color);
                    }
                    if let Some(url) = &span.style.link {
                        canvas.fill(Rect::new(rect.x, rect.y + size + 1, rect.w, 1), *color);
                        link_hit(rect, url.clone());
                    }
                }
            }
        }
    }
}

struct Builder<'a> {
    font: &'a Type,
    layout: Layout,
}

impl Builder<'_> {
    fn fill(&mut self, rect: Rect, color: u32) {
        self.layout.width = self.layout.width.max(rect.x + rect.w);
        self.layout.height = self.layout.height.max(rect.y + rect.h);
        self.layout.commands.push(Draw::Fill(rect, color));
    }

    fn text(&mut self, span: Span, x: i32, y: i32, size: i32, color: u32) {
        let width = self.font.styled_width(&span.text, size, span.style.text);
        let rect = Rect::new(x, y, width, size + 5);
        self.layout.height = self.layout.height.max(y + rect.h);
        self.layout.width = self.layout.width.max(x + width);
        let color = if span.style.link.is_some() {
            LINK
        } else {
            color
        };
        self.layout
            .commands
            .push(Draw::Text(rect, size, span, color));
    }

    fn plain(&mut self, text: String, x: i32, y: i32, color: u32) {
        self.text(
            Span {
                text,
                style: Style::default(),
            },
            x,
            y,
            BODY,
            color,
        );
    }

    fn blocks(&mut self, nodes: &[Node], x: i32, width: i32, mut y: i32) -> i32 {
        let mut pending = Vec::new();
        for node in nodes {
            match node {
                Node::Group(tag, children) if is_block(tag) => {
                    y = self.paragraph(&mut pending, x, width, y);
                    y = self.block(tag, children, x, width, y);
                }
                Node::Leaf(Event::Rule) => {
                    y = self.paragraph(&mut pending, x, width, y);
                    self.fill(Rect::new(x, y + 4, width, 1), LINE);
                    y += 15;
                }
                _ => inline(std::slice::from_ref(node), &mut pending, &Style::default()),
            }
        }
        self.paragraph(&mut pending, x, width, y)
    }

    fn paragraph(&mut self, spans: &mut Vec<Span>, x: i32, width: i32, y: i32) -> i32 {
        if spans.is_empty() {
            return y;
        }
        self.flow(
            spans,
            Rect::new(x, y, width, 0),
            BODY,
            INK,
            false,
            Alignment::Left,
        ) + 7
    }

    fn block(&mut self, tag: &Tag<'_>, nodes: &[Node], x: i32, width: i32, y: i32) -> i32 {
        match tag {
            Tag::Heading { level, .. } => {
                let size = match level {
                    HeadingLevel::H1 => 25,
                    HeadingLevel::H2 => 22,
                    HeadingLevel::H3 => 19,
                    _ => 17,
                };
                let mut spans = Vec::new();
                inline(nodes, &mut spans, &bold());
                self.flow(
                    &mut spans,
                    Rect::new(x, y, width, 0),
                    size,
                    INK,
                    false,
                    Alignment::Left,
                ) + 7
            }
            Tag::BlockQuote(_) if width >= 40 => {
                let start = self.layout.commands.len();
                let end = self.blocks(nodes, x + 14, width - 14, y + 3).max(y + 21);
                self.fill(Rect::new(x, y, 3, end - y - 3), LINE);
                self.layout.commands[start..].rotate_right(1);
                end + 2
            }
            Tag::List(start) => self.list(nodes, *start, x, width, y),
            Tag::CodeBlock(kind) => self.code(nodes, kind, x, width, y),
            Tag::Table(alignments) => self.table(nodes, alignments, x, width, y),
            _ => self.blocks(nodes, x, width, y),
        }
    }

    fn list(&mut self, nodes: &[Node], start: Option<u64>, x: i32, width: i32, mut y: i32) -> i32 {
        let marker_width = match start {
            Some(start) => {
                self.font.width(
                    &format!("{}.", start.saturating_add(nodes.len() as u64)),
                    BODY,
                ) + 7
            }
            None => 17,
        }
        .min((width - 16).max(0));
        for (index, node) in nodes.iter().enumerate() {
            if let Node::Group(Tag::Item, children) = node {
                let marker = start.map_or_else(
                    || "•".into(),
                    |n| format!("{}.", n.saturating_add(index as u64)),
                );
                if marker_width > 0 {
                    self.plain(marker, x, y, MUTED);
                }
                y = self
                    .blocks(children, x + marker_width, width - marker_width, y)
                    .max(y + BODY + 5);
            }
        }
        y + 2
    }

    fn code(
        &mut self,
        nodes: &[Node],
        kind: &CodeBlockKind<'_>,
        x: i32,
        width: i32,
        y: i32,
    ) -> i32 {
        let start = self.layout.commands.len();
        let mut top = y + 7;
        if let CodeBlockKind::Fenced(info) = kind
            && let Some(language) = info.split_whitespace().next()
        {
            let label = self.font.ellipsis(language, width - 16, 12);
            self.text(
                Span {
                    text: label,
                    style: bold(),
                },
                x + 8,
                top,
                12,
                MUTED,
            );
            top += 18;
        }
        let style = Style {
            text: TextStyle {
                mono: true,
                ..TextStyle::default()
            },
            ..Style::default()
        };
        let mut spans = Vec::new();
        inline(nodes, &mut spans, &style);
        for span in &mut spans {
            span.text = span.text.replace('\t', "    ");
        }
        if let Some(last) = spans.last_mut()
            && last.text.ends_with('\n')
        {
            last.text.pop();
        }
        let bottom = self.flow(
            &mut spans,
            Rect::new(x + 8, top, width - 16, 0),
            14,
            INK,
            true,
            Alignment::Left,
        ) + 7;
        self.fill(Rect::new(x, y, width, bottom - y), CODE);
        self.layout.commands[start..].rotate_right(1);
        bottom + 7
    }

    fn table(
        &mut self,
        rows: &[Node],
        alignments: &[Alignment],
        x: i32,
        width: i32,
        mut y: i32,
    ) -> i32 {
        if width < MIN_COLUMN * alignments.len() as i32 {
            return self.stacked_table(rows, x, width, y);
        }
        let columns = self.table_columns(rows, alignments.len(), width);
        for (index, row) in rows.iter().enumerate() {
            let Some((cells, header)) = table_row(row) else {
                continue;
            };
            let start = self.layout.commands.len();
            let mut end = y + 29;
            for ((cell, alignment), edges) in cells.iter().zip(alignments).zip(columns.windows(2)) {
                let column_x = x + edges[0];
                let cell_width = edges[1] - edges[0];
                let spans = &mut table_cell(cell, header);
                end = end.max(
                    self.flow(
                        spans,
                        Rect::new(column_x + 7, y + 6, cell_width - 14, 0),
                        14,
                        INK,
                        false,
                        *alignment,
                    ) + 5,
                );
            }
            self.fill(
                Rect::new(x, y, width, end - y),
                if header {
                    TINT
                } else if index % 2 == 0 {
                    CODE
                } else {
                    0xfffafbf9
                },
            );
            self.layout.commands[start..].rotate_right(1);
            self.fill(Rect::new(x, end - 1, width, 1), LINE);
            for &edge in &columns {
                self.fill(Rect::new(x + edge.min(width - 1), y, 1, end - y), LINE);
            }
            y = end;
        }
        y + 7
    }

    fn table_columns(&self, rows: &[Node], count: usize, width: i32) -> Vec<i32> {
        let ceiling = (width * 3 / 5).max(MIN_COLUMN);
        let equal_share = width / count as i32;
        let mut minimum = vec![MIN_COLUMN; count];
        let mut preferred = vec![MIN_COLUMN; count];
        for (cells, header) in rows.iter().filter_map(table_row) {
            for ((cell, minimum), desired) in cells.iter().zip(&mut minimum).zip(&mut preferred) {
                let spans = table_cell(cell, header);
                let mut total = 0;
                for span in &spans {
                    total += self.font.styled_width(&span.text, 14, span.style.text);
                    for word in span.text.split_whitespace() {
                        let word = self.font.styled_width(word, 14, span.style.text);
                        *minimum = (*minimum).max((word + 14).min(equal_share));
                    }
                }
                *desired = (*desired).max((total + 14).min(ceiling));
            }
        }
        let weights = preferred
            .iter()
            .zip(&minimum)
            .map(|(desired, minimum)| (desired - minimum).max(0));
        let total_weight = weights.clone().sum::<i32>();
        let available = width - minimum.iter().sum::<i32>();
        let mut edges = vec![0];
        let mut share = 0;
        // Compute from cumulative weight so rounding is shared across columns.
        let mut used = 0;
        for (minimum, weight) in minimum.iter().zip(weights) {
            used += if total_weight == 0 { 1 } else { weight };
            share += minimum;
            edges.push(
                share
                    + available * used
                        / if total_weight == 0 {
                            count as i32
                        } else {
                            total_weight
                        },
            );
        }
        edges
    }

    fn stacked_table(&mut self, rows: &[Node], x: i32, width: i32, mut y: i32) -> i32 {
        let mut headers = &[][..];
        for row in rows {
            let Some((cells, header)) = table_row(row) else {
                continue;
            };
            if header {
                headers = cells;
                continue;
            }
            self.fill(Rect::new(x, y, width, 1), LINE);
            y += 6;
            for (column, cell) in cells.iter().enumerate() {
                let mut spans = headers
                    .get(column)
                    .map_or_else(Vec::new, |head| table_cell(head, true));
                if !spans.is_empty() {
                    add_span(&mut spans, "  ", &Style::default());
                }
                spans.extend(table_cell(cell, false));
                y = self.flow(
                    &mut spans,
                    Rect::new(x + 5, y, width - 10, 0),
                    14,
                    INK,
                    false,
                    Alignment::Left,
                ) + 3;
            }
        }
        if !headers.is_empty() && rows.len() == 1 {
            for cell in headers {
                y = self.flow(
                    &mut table_cell(cell, true),
                    Rect::new(x + 5, y, width - 10, 0),
                    14,
                    INK,
                    false,
                    Alignment::Left,
                );
            }
        }
        y + 7
    }

    fn flow(
        &mut self,
        spans: &mut Vec<Span>,
        rect: Rect,
        size: i32,
        color: u32,
        preserve: bool,
        align: Alignment,
    ) -> i32 {
        let mut lines = Lines::new(self.font, rect.w.max(1), size, preserve);
        for span in spans.drain(..) {
            lines.push(span);
        }
        let rows = lines.finish();
        let mut y = rect.y;
        for row in rows {
            let remaining = (rect.w - row.width).max(0);
            let mut x = rect.x
                + match align {
                    Alignment::Right => remaining,
                    Alignment::Center => remaining / 2,
                    _ => 0,
                };
            for span in row.spans {
                let advance = self.font.styled_width(&span.text, size, span.style.text);
                self.text(span, x, y, size, color);
                x += advance;
            }
            y += size + 5;
        }
        y
    }
}

fn bold() -> Style {
    Style {
        text: TextStyle {
            bold: true,
            ..TextStyle::default()
        },
        ..Style::default()
    }
}

fn table_row(row: &Node) -> Option<(&[Node], bool)> {
    match row {
        Node::Group(Tag::TableHead, cells) => Some((cells, true)),
        Node::Group(Tag::TableRow, cells) => Some((cells, false)),
        _ => None,
    }
}

fn table_cell(cell: &Node, header: bool) -> Vec<Span> {
    let mut spans = Vec::new();
    if let Node::Group(Tag::TableCell, nodes) = cell {
        let style = if header { bold() } else { Style::default() };
        inline(nodes, &mut spans, &style);
    }
    spans
}

fn is_block(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::List(_)
            | Tag::Table(_)
            | Tag::HtmlBlock
    )
}

#[derive(Default)]
struct Row {
    spans: Vec<Span>,
    width: i32,
}

struct Lines<'a> {
    font: &'a Type,
    width: i32,
    size: i32,
    preserve: bool,
    rows: Vec<Row>,
    row: Row,
    word: Vec<Span>,
    space: Option<Style>,
}

impl<'a> Lines<'a> {
    fn new(font: &'a Type, width: i32, size: i32, preserve: bool) -> Self {
        Self {
            font,
            width,
            size,
            preserve,
            rows: Vec::new(),
            row: Row::default(),
            word: Vec::new(),
            space: None,
        }
    }

    fn push(&mut self, span: Span) {
        let mut word = String::new();
        for ch in span.text.chars() {
            if ch == '\n' || (!self.preserve && ch.is_whitespace() && ch != '\u{a0}') {
                add_span(&mut self.word, &word, &span.style);
                word.clear();
                self.flush_word();
                if ch == '\n' {
                    self.line();
                } else {
                    self.space = Some(span.style.clone());
                }
            } else if self.preserve && ch == ' ' {
                word.push(ch);
                add_span(&mut self.word, &word, &span.style);
                word.clear();
                self.flush_word();
            } else {
                word.push(ch);
            }
        }
        add_span(&mut self.word, &word, &span.style);
    }

    fn finish(mut self) -> Vec<Row> {
        self.flush_word();
        if !self.row.spans.is_empty() {
            self.line();
        }
        self.rows
    }

    fn line(&mut self) {
        self.rows.push(std::mem::take(&mut self.row));
        self.space = None;
    }

    fn flush_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.word);
        let width: i32 = runs
            .iter()
            .map(|run| self.measure(&run.text, &run.style))
            .sum();
        let space = self.space.take().filter(|_| self.row.width > 0);
        let gap = space.as_ref().map_or(0, |style| self.measure(" ", style));
        if self.row.width > 0 && self.row.width + gap + width > self.width {
            self.line();
        } else if let Some(style) = space {
            self.append(" ", &style);
        }
        for run in runs {
            if width <= self.width {
                self.append(&run.text, &run.style);
            } else {
                let mut buffer = [0; 4];
                for ch in run.text.chars() {
                    let letter = ch.encode_utf8(&mut buffer);
                    if self.row.width > 0
                        && self.row.width + self.measure(letter, &run.style) > self.width
                    {
                        self.line();
                    }
                    self.append(letter, &run.style);
                }
            }
        }
    }

    fn measure(&self, text: &str, style: &Style) -> i32 {
        self.font.styled_width(text, self.size, style.text)
    }

    fn append(&mut self, text: &str, style: &Style) {
        let previous = self
            .row
            .spans
            .last()
            .filter(|span| span.style == *style)
            .map_or(0, |span| self.measure(&span.text, style));
        add_span(&mut self.row.spans, text, style);
        let span = self.row.spans.last().expect("appended text span");
        self.row.width += self.measure(&span.text, style) - previous;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_runs(layout: &Layout) -> impl Iterator<Item = (&Rect, &Span)> {
        layout.commands.iter().filter_map(|draw| match draw {
            Draw::Text(rect, _, span, _) => Some((rect, span)),
            Draw::Fill(_, _) => None,
        })
    }

    #[test]
    fn lays_out_rich_messages_and_keeps_unsafe_content_inert() {
        let body = concat!(
            "# Heading\n\n> **Bold and *italic***, ~~old~~ and `code`.\n\n",
            "7. Start here\n   - [x] Nested task\n\n",
            "| Status | Link |\n| :--- | ---: |\n| Ready | [valid][reference] |\n\n",
            "[reference]: https://example.org/doc\n\n",
            "[unsafe](file:///private) ![alt](https://example.org/remote.png) ",
            "<img src=x>\n\n```rust\n    let answer = 42;\n```",
        );
        let font = Type::load().expect("embedded font");
        for width in [96, 480] {
            let layout = Layout::message(body, width, &font);
            let runs: Vec<_> = text_runs(&layout).collect();
            let contains = |text: &str| runs.iter().any(|(_, span)| span.text.contains(text));
            assert!(layout.width <= width);
            assert!(runs.iter().all(|(rect, _)| rect.x + rect.w <= width));
            assert!(
                runs.iter()
                    .any(|(_, span)| span.style.text.bold && span.style.text.italic)
            );
            assert!(
                runs.iter()
                    .any(|(_, span)| span.style.strike && span.text == "old")
            );
            assert!(
                runs.iter()
                    .any(|(_, span)| span.style.text.mono && span.text.starts_with("    let"))
            );
            assert!(contains("7.") && contains("[x]") && contains("Ready"));
            assert!(contains("unsafe") && contains("Image:") && contains("<img"));
            let links: Vec<_> = runs
                .iter()
                .filter_map(|(_, span)| span.style.link.as_deref())
                .collect();
            assert!(!links.is_empty());
            assert!(links.iter().all(|url| *url == "https://example.org/doc"));
        }
    }

    #[test]
    fn deep_markdown_falls_back_to_readable_text() {
        let input = format!("{}Still visible", "> ".repeat(1000));
        let font = Type::load().expect("embedded font");
        let layout = Layout::message(&input, 200, &font);
        let visible = text_runs(&layout)
            .map(|(_, span)| span.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(visible.contains("Still visible"));
    }
}
