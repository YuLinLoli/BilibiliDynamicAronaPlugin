//! 字体发现、文本测量与排版。
//!
//! 卡片要写中文，Windows 上是 `msyh.ttc`、Linux 容器里多是 `Noto Sans CJK`，所以字体只能从系统里找，
//! 由 `bili-image.font` 覆盖。字体文件不打包进 dll——中文字体动辄十几 MB，插件不该带这份体积。
//!
//! 排版自己算而不用 `fontdue::layout`：它的换行按空格断词，中文整段不带空格会被排成一行。
//! 这里按"中日韩逐字可断、拉丁字母聚成词、`[表情]` 当一个方块"来断行，正好也是 B 站页面的规则。
//!
//! 行距由调用方给（原插件是 `字号 + lineSpace` 的固定口径），不取字体自带行高：
//! 这样四档画质的观感与原插件一致，也不会因为换了字体文件而跳行距。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::{Font, FontSettings};
use image::RgbaImage;

use super::canvas::{Canvas, Color};

/// B 站表情的写法是 `[名字]`，名字没有超过这个长度的
const EMOJI_MAX_LEN: usize = 16;

/// 一次加载好的常规 + 粗体两份字面
pub struct Typeface {
    regular: Font,
    bold: Option<Font>,
}

/// 按配置路径缓存字面：热重载不该每次绘图都读一遍十几 MB 的字体
static CACHE: OnceLock<Mutex<HashMap<String, Arc<Typeface>>>> = OnceLock::new();

/// 一小段同色文字。正文里话题、链接、@ 各占一段，所以排版收 Span 而不是整串文本。
#[derive(Clone, Copy, Debug)]
pub struct Span<'a> {
    pub text: &'a str,
    pub color: Color,
}

impl<'a> Span<'a> {
    pub fn new(text: &'a str, color: Color) -> Span<'a> {
        Span { text, color }
    }
}

/// 断行后的最小一格：一段文字或一个表情方块。宽度在断行时算好带着走，
/// 这样量高和真画两遍用的是同一个数。
#[derive(Clone, Debug)]
struct Token {
    text: String,
    color: Color,
    /// 有值表示这格该按表情图画
    emoji: Option<String>,
    width: f32,
}

/// 断好行的一段文字
#[derive(Clone, Debug, Default)]
pub struct Flow {
    rows: Vec<Vec<Token>>,
}

impl Flow {
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// 最宽那一行的宽度：定徽章、居中之类的地方要拿它当文本宽
    pub fn width(&self) -> f32 {
        self.rows
            .iter()
            .map(|row| row.iter().map(|token| token.width).sum::<f32>())
            .fold(0.0f32, f32::max)
    }
}

/// 表情名（`[笑]` 这种带方括号的原文）→ 图。绘图层只认这份，不负责下载。
#[derive(Clone, Default)]
pub struct EmojiSet(HashMap<String, RgbaImage>);

impl EmojiSet {
    pub fn new(map: HashMap<String, RgbaImage>) -> EmojiSet {
        EmojiSet(map)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn get(&self, name: &str) -> Option<&RgbaImage> {
        self.0.get(name)
    }
}

impl Typeface {
    /// `configured` 是 `bili-image.font` 的值：常规字体路径，可用 `;` 再跟一个粗体路径。
    /// 留空则按系统候选路径找。找不到任何中文字体就报错，由调用方退回纯文本推送。
    pub fn discover(configured: &str) -> Result<Arc<Typeface>, String> {
        let key = configured.trim().to_string();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(found) = cache.lock().ok().and_then(|map| map.get(&key).cloned()) {
            return Ok(found);
        }
        let (regular_path, bold_path) = resolve(&key)?;
        let regular = open(&regular_path)?;
        let bold = bold_path
            .filter(|path| path.is_file())
            .map(|path| open(&path))
            .transpose()?;
        let face = Arc::new(Typeface { regular, bold });
        if let Ok(mut map) = cache.lock() {
            map.insert(key, Arc::clone(&face));
        }
        Ok(face)
    }

    fn face(&self, bold: bool) -> &Font {
        if bold {
            self.bold.as_ref().unwrap_or(&self.regular)
        } else {
            &self.regular
        }
    }

    fn width_of(&self, text: &str, px: f32, bold: bool) -> f32 {
        let face = self.face(bold);
        text.chars()
            .map(|ch| face.metrics(ch, px).advance_width)
            .sum()
    }

    /// 一行文字的宽度（表情按一个方块算）
    pub fn measure(&self, text: &str, px: f32, bold: bool, emojis: &EmojiSet) -> f32 {
        units(text)
            .iter()
            .map(|unit| unit.width(self.face(bold), px, emojis))
            .sum()
    }

    /// 单行截断：放不下就砍到 max_width 并以省略号结尾
    pub fn truncate(&self, text: &str, px: f32, bold: bool, max_width: f32) -> String {
        let flat = text.replace('\n', " ");
        if self.width_of(&flat, px, bold) <= max_width {
            return flat;
        }
        let face = self.face(bold);
        let reserve = face.metrics('…', px).advance_width;
        let mut out = String::new();
        let mut used = 0.0f32;
        for ch in flat.chars() {
            let width = face.metrics(ch, px).advance_width;
            if used + width + reserve > max_width {
                break;
            }
            used += width;
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// 断行。多个 Span 是同一段里的连续片段，跨 Span 不断行；`\n` 强制断行。
    /// `max_lines` 给了就把超出的部分收成省略号。
    pub fn layout(
        &self,
        spans: &[Span],
        px: f32,
        bold: bool,
        max_width: f32,
        max_lines: Option<usize>,
        emojis: &EmojiSet,
    ) -> Flow {
        let face = self.face(bold);
        let mut rows: Vec<Vec<Token>> = Vec::new();
        let mut current: Vec<Token> = Vec::new();
        let mut used = 0.0f32;
        for span in spans {
            for (index, part) in span.text.split('\n').enumerate() {
                if index > 0 {
                    used = 0.0;
                    trim_tail(&mut current);
                    rows.push(std::mem::take(&mut current));
                }
                for unit in units(part) {
                    let width = unit.width(face, px, emojis);
                    if used + width > max_width && !current.is_empty() {
                        used = 0.0;
                        trim_tail(&mut current);
                        rows.push(std::mem::take(&mut current));
                    }
                    used += width;
                    current.push(Token {
                        text: unit.text,
                        color: span.color,
                        emoji: unit.emoji,
                        width,
                    });
                }
            }
        }
        trim_tail(&mut current);
        if !current.is_empty() || rows.is_empty() {
            rows.push(current);
        }
        if let Some(limit) = max_lines {
            if rows.len() > limit {
                rows.truncate(limit);
                let reserve = face.metrics('…', px).advance_width;
                let base_color = spans
                    .first()
                    .map(|span| span.color)
                    .unwrap_or_else(|| Color::rgb(0x22, 0x22, 0x22));
                if let Some(row) = rows.last_mut() {
                    let mut width: f32 = row.iter().map(|token| token.width).sum();
                    while width + reserve > max_width {
                        match row.pop() {
                            Some(dropped) => width -= dropped.width,
                            None => break,
                        }
                    }
                    row.push(Token {
                        text: "…".to_string(),
                        color: base_color,
                        emoji: None,
                        width: reserve,
                    });
                }
            }
        }
        Flow { rows }
    }

    /// 画满这么多行要占多高（含末行的行距），排版时先量后画
    pub fn height(&self, flow: &Flow, leading: f32) -> f32 {
        flow.rows() as f32 * leading
    }

    /// 从 `first_baseline` 开始逐行画下去，返回下一段的基线位置
    pub fn draw(
        &self,
        canvas: &mut Canvas,
        flow: &Flow,
        x: f32,
        first_baseline: f32,
        px: f32,
        bold: bool,
        leading: f32,
        emojis: &EmojiSet,
    ) -> f32 {
        let face = self.face(bold);
        let mut baseline = first_baseline;
        for row in &flow.rows {
            let mut pen = x;
            for token in row {
                let side = px.round().max(1.0) as u32;
                match token
                    .emoji
                    .as_ref()
                    .and_then(|name| emojis.get(name))
                    .map(|src| super::canvas::resize(src, side, side))
                {
                    // 表情底边坐到基线，稍微下沉才跟中文字形齐
                    Some(scaled) => canvas.paste(
                        pen.round() as u32,
                        (baseline - px * 0.88).round() as u32,
                        &scaled,
                    ),
                    // 图没下下来就照文字画，`[脱单delta]` 在群里也能读
                    None => glyphs(canvas, face, pen, baseline, &token.text, px, token.color),
                }
                pen += token.width;
            }
            baseline += leading;
        }
        baseline
    }

    /// 徽章、页脚那种单行文字：不换行，返回这行占的宽度
    pub fn draw_line(
        &self,
        canvas: &mut Canvas,
        x: f32,
        baseline: f32,
        text: &str,
        px: f32,
        color: Color,
        bold: bool,
    ) -> f32 {
        glyphs(canvas, self.face(bold), x, baseline, text, px, color);
        self.width_of(text, px, bold)
    }
}

/// 断行的最小单元：一整块不可再断的文字（拉丁词、单个符号、单个汉字）或一个表情方块
struct Unit {
    text: String,
    emoji: Option<String>,
}

impl Unit {
    fn width(&self, face: &Font, px: f32, emojis: &EmojiSet) -> f32 {
        match &self.emoji {
            // 有图就按正方形一格占位，跟 draw 里贴图的口径一致
            Some(name) if emojis.contains(name) => px,
            _ => self
                .text
                .chars()
                .map(|ch| face.metrics(ch, px).advance_width)
                .sum(),
        }
    }
}

/// 把一行切成单元：连续的拉丁字母数字成一词，其余每个字符单独成块
fn units(text: &str) -> Vec<Unit> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut chunk = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '[' {
            if let Some(end) = find_emoji(&chars, index) {
                if !chunk.is_empty() {
                    out.push(word(std::mem::take(&mut chunk)));
                }
                out.push(emoji(chars[index..=end].iter().collect()));
                index = end + 1;
                continue;
            }
        }
        if ch.is_ascii_alphanumeric() {
            chunk.push(ch);
            index += 1;
            continue;
        }
        if !chunk.is_empty() {
            out.push(word(std::mem::take(&mut chunk)));
        }
        out.push(word(ch.to_string()));
        index += 1;
    }
    if !chunk.is_empty() {
        out.push(word(chunk));
    }
    out
}

fn word(text: String) -> Unit {
    Unit { text, emoji: None }
}

fn emoji(text: String) -> Unit {
    Unit {
        emoji: Some(text.clone()),
        text,
    }
}

/// `[` 往后找到配对的 `]`，返回 `]` 的下标
fn find_emoji(chars: &[char], start: usize) -> Option<usize> {
    let tail = chars.get(start + 1..)?;
    let limit = tail.len().min(EMOJI_MAX_LEN);
    for offset in 0..limit {
        match tail[offset] {
            ']' if offset >= 1 => return Some(start + offset + 1),
            '[' | '\n' => return None,
            _ => {}
        }
    }
    None
}

/// 去掉行尾空白：中文排版里行尾不该留白
fn trim_tail(row: &mut Vec<Token>) {
    while let Some(last) = row.last() {
        if last.emoji.is_none() && last.text.trim().is_empty() {
            row.pop();
            continue;
        }
        break;
    }
    if let Some(last) = row.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
}

/// 字形按覆盖度混进画布
fn glyphs(
    canvas: &mut Canvas,
    face: &Font,
    x: f32,
    baseline: f32,
    text: &str,
    px: f32,
    color: Color,
) {
    let mut pen = x;
    for ch in text.chars() {
        let (metrics, coverage) = face.rasterize(ch, px);
        let left = pen.round() as i32 + metrics.xmin;
        let top = baseline.round() as i32 - metrics.height as i32 + metrics.ymin;
        for row in 0..metrics.height {
            for column in 0..metrics.width {
                let alpha = coverage[row * metrics.width + column];
                let x = left + column as i32;
                let y = top + row as i32;
                if alpha > 0 && x >= 0 && y >= 0 {
                    canvas.blend_pixel(x as u32, y as u32, color, alpha);
                }
            }
        }
        pen += metrics.advance_width;
    }
}

/// 字体候选路径：Windows 上雅黑/黑体，容器里多是 Noto CJK，macOS 用苹方。
/// 每项是（常规, 可选粗体）；字体文件不打包进 dll，中文字体动辄十几 MB。
const CANDIDATES: &[(&str, Option<&str>)] = &[
    (
        r"C:\Windows\Fonts\msyh.ttc",
        Some(r"C:\Windows\Fonts\msyhbd.ttc"),
    ),
    (r"C:\Windows\Fonts\simhei.ttf", None),
    (
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        Some("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"),
    ),
    (
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        None,
    ),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", None),
    ("/usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc", None),
    ("/System/Library/Fonts/PingFang.ttc", None),
];

/// 配置留空时在系统里找；配置给了路径就只认它（粗体可选，`;` 或 `,` 分隔）。
fn resolve(configured: &str) -> Result<(PathBuf, Option<PathBuf>), String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let (main, bold) = match configured.split_once([';', ',']) {
            Some((main, bold)) => (main.trim().to_string(), Some(bold.trim().to_string())),
            None => (configured.to_string(), None),
        };
        let path = PathBuf::from(&main);
        return match path.is_file() {
            true => Ok((path, bold.map(PathBuf::from))),
            false => Err(format!("配置里指定的字体不存在：{main}")),
        };
    }
    CANDIDATES
        .iter()
        .filter_map(|(regular, bold)| {
            let path = PathBuf::from(regular);
            path.is_file().then(|| (path, bold.map(PathBuf::from)))
        })
        .next()
        .ok_or_else(|| {
            "系统里找不到中文字体，请在 config/bilibili/arona.yml 的 bili-image.font 指定字体文件路径"
                .to_string()
        })
}

/// 读一个字体文件。`.ttc` 是字体集合，第 0 个 face 就是常规字重。
fn open(path: &Path) -> Result<Font, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("读字体 {} 失败：{err}", path.display()))?;
    let settings = FontSettings {
        collection_index: 0,
        scale: 40.0,
        ..Default::default()
    };
    Font::from_bytes(bytes, settings)
        .map_err(|err| format!("字体 {} 解析失败：{err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_scan_pairs_only_the_nearest_bracket() {
        let nested: Vec<char> = "[a[b]".chars().collect();
        assert_eq!(find_emoji(&nested, 0), None);
        let ok: Vec<char> = "[笑]尾巴".chars().collect();
        assert_eq!(find_emoji(&ok, 0), Some(2));
    }

    #[test]
    fn latin_words_stay_whole_while_cjk_breaks_per_char() {
        let parts = units("hello 你好[笑]");
        let texts: Vec<&str> = parts.iter().map(|unit| unit.text.as_str()).collect();
        assert_eq!(texts, vec!["hello", " ", "你", "好", "[笑]"]);
    }

    #[test]
    fn urls_break_at_punctuation_not_mid_word() {
        let parts = units("https://t.bilibili.com/123");
        let texts: Vec<&str> = parts.iter().map(|unit| unit.text.as_str()).collect();
        // 连续的字母数字成词，标点单字成块——断行只会落在标点处
        assert_eq!(
            texts,
            vec![
                "https", ":", "/", "/", "t", ".", "bilibili", ".", "com", "/", "123"
            ]
        );
    }

    #[test]
    fn missing_font_reports_a_readable_reason() {
        let error = resolve("Z:\\不存在\\字体.ttf").unwrap_err();
        assert!(error.contains("不存在"), "{error}");
    }

    #[test]
    fn empty_and_plain_paths_both_resolve() {
        // 本机（Windows）上一定有雅黑；跑在容器里则按候选路径找，两边都不该报"配置指定"的错
        let (regular, _bold) = resolve("").unwrap();
        assert!(
            regular.is_file(),
            "候选列表里得有一个真存在的字体：{}",
            regular.display()
        );
    }

    #[test]
    fn flow_breaks_long_cjk_within_the_box() {
        let face = Typeface::discover("").unwrap();
        let pink = Color::rgb(0xFB, 0x72, 0x99);
        let text = "这是一段很长的中文正文，用来验证断行不会把字画到框外面去，超出宽度就得另起一行继续排。";
        let flow = face.layout(
            &[Span::new(text, pink)],
            32.0,
            false,
            200.0,
            None,
            &EmojiSet::default(),
        );
        assert!(
            flow.rows() > 3,
            "200 宽的框该断成好几行，实得 {}",
            flow.rows()
        );
        assert!(
            flow.width() <= 200.0,
            "有一行顶破了 max_width：{}",
            flow.width()
        );
    }

    #[test]
    fn flow_over_the_limit_ends_with_an_ellipsis() {
        let face = Typeface::discover("").unwrap();
        let black = Color::rgb(0x22, 0x22, 0x22);
        let text = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十";
        let flow = face.layout(
            &[Span::new(text, black)],
            32.0,
            false,
            100.0,
            Some(2),
            &EmojiSet::default(),
        );
        assert_eq!(flow.rows(), 2);
        let last = flow.rows.last().unwrap();
        assert_eq!(last.last().map(|token| token.text.as_str()), Some("…"));
        assert!(
            last.iter().map(|token| token.width).sum::<f32>() <= 100.0,
            "省略号那行还超宽"
        );
    }

    #[test]
    fn hard_newline_starts_a_new_row() {
        let face = Typeface::discover("").unwrap();
        let black = Color::rgb(0x22, 0x22, 0x22);
        let flow = face.layout(
            &[Span::new("第一行\n第二行", black)],
            32.0,
            false,
            800.0,
            None,
            &EmojiSet::default(),
        );
        assert_eq!(flow.rows(), 2);
    }

    #[test]
    fn spans_stay_on_one_row_until_the_width_runs_out() {
        let face = Typeface::discover("").unwrap();
        let blue = Color::rgb(0x17, 0x8B, 0xCF);
        let black = Color::rgb(0x22, 0x22, 0x22);
        let spans = [
            Span::new("#话题#", blue),
            Span::new("正文正文正文正文正文", black),
        ];
        // 话题和正文是同一段里的连续片段，不该因为换了颜色就断行
        let wide = face.layout(&spans, 32.0, false, 600.0, None, &EmojiSet::default());
        assert_eq!(wide.rows(), 1, "放得下就不该断行，实得 {} 行", wide.rows());
        let colors: Vec<Color> = wide
            .rows
            .iter()
            .flatten()
            .map(|token| token.color)
            .collect();
        assert_eq!(colors.first(), Some(&blue), "段首该是话题色");
        assert!(colors.contains(&black), "正文色被丢了");
        // 收窄到放不下才断，断点是宽度而不是换色
        let tight = face.layout(&spans, 32.0, false, 300.0, None, &EmojiSet::default());
        assert!(tight.rows() > 1, "300 宽的框没断行：{}", tight.rows());
    }
}
