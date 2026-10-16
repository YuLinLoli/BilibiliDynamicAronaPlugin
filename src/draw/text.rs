//! 字体发现、文本测量与排版。
//!
//! 字体优先要框架随包的那一份（`arona::runtime::assets::cjk_font()`，安装目录 `fonts/` 下的
//! 思源宋体简中），拿不到才退回系统字体目录那串候选，`bili-image.font` 再覆盖两者。
//! 这样 GUI 与卡片用的是同一套字，观感不随"这台机器装了什么字体"漂；插件也不带字体体积。
//!
//! 栅格化走 ab_glyph 的惰性后端：内存里只留字体文件的原始字节（思源宋体常规 11.1 MB、
//! 粗体 11.5 MB），画到哪个字才解析哪个字，栅格结果进一层有界 LRU。换掉的是 fontdue
//! 那种"把每个码位的轮廓一次全解析完"的路子——实测同一份雅黑常驻 160.6 MB、思源宋体
//! 253.5 MB，装载还要 0.3~0.6 s；惰性那一档装载 0.01 s、常驻 48 MB（含文件字节）。
//!
//! 排版自己算而不用现成的 layout 器：它们按空格断词，中文整段不带空格会被排成一行。
//! 这里按"中日韩逐字可断、拉丁字母聚成词、`[表情]` 当一个方块"来断行，正好也是 B 站页面的规则。
//!
//! 行距由调用方给（原插件是 `字号 + lineSpace` 的固定口径），不取字体自带行高：
//! 这样四档画质的观感与原插件一致，也不会因为换了字体文件而跳行距。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use ab_glyph::{Font as _, Glyph, ScaleFont};
use image::RgbaImage;

use super::canvas::{Canvas, Color};

/// B 站表情的写法是 `[名字]`，名字没有超过这个长度的
const EMOJI_MAX_LEN: usize = 16;

/// 字形缓存的上限。一张卡实际用到的（字符, 字号）组合在几百到一两千这一档，
/// 4096 条按 1~2 KB 算是几 MB——比全量预解析的 160~250 MB 差两个数量级，
/// 又足够让一整轮里反复出现的字（标点、数字、"更新维护说明"那类高频词）都命中。
const GLYPH_CACHE_MAX: usize = 4096;

/// 一次栅格化的位置与大小。字段口径沿用 fontdue 的 `Metrics`：
/// `xmin`/`ymin` 是相对笔位置（水平笔位、垂直基线）的左边界与**下**边界，y 向上为正。
/// 画布那一路按这套算落点，换后端时不必动贴图代码。
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    pub xmin: i32,
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
    pub advance_width: f32,
}

/// 一个栅格好的字形：落点 + 覆盖率位图。缓存里存的就是它，取用只加一次引用
type Cached = Arc<(GlyphMetrics, Vec<u8>)>;

/// 一份字面：原始字节 + 一层按 (字符, 字号) 计的字形缓存
pub struct Face {
    font: ab_glyph::FontArc,
    glyphs: Mutex<Glyphs>,
}

/// 有界的字形 LRU。顺序表记"最近用过"，到顶从头上摘最久没用的那一条；
/// 上限只有几千，线性挪位的开销比栅格化本身低两个数量级
struct Glyphs {
    map: HashMap<(char, u32), Cached>,
    order: VecDeque<(char, u32)>,
}

impl Glyphs {
    /// 命中就把这条挪到队尾（口径与 `petpet` 那份素材缓存一致）
    fn touch(&mut self, key: (char, u32)) {
        if let Some(at) = self.order.iter().position(|have| *have == key) {
            self.order.remove(at);
        }
        self.order.push_back(key);
    }

    /// 装满了就先丢最久没被用的那条，再放新的
    fn put(&mut self, key: (char, u32), done: Cached) {
        while self.map.len() >= GLYPH_CACHE_MAX && self.map.len() > 1 {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
        self.map.insert(key, done);
        self.touch(key);
    }
}

impl Face {
    /// 一个字占多宽（排版只用这一个数，不必解析轮廓）
    pub fn advance(&self, ch: char, px: f32) -> f32 {
        let scaled = self.font.as_scaled(self.scale(px));
        scaled.h_advance(scaled.glyph_id(ch))
    }

    /// 一个字的覆盖率位图（0~255），加它的落点
    pub fn rasterize(&self, ch: char, px: f32) -> (GlyphMetrics, Vec<u8>) {
        let done = self.raster(ch, px);
        (done.0, done.1.clone())
    }

    /// 把"字号 = em 的像素数"换成 ab_glyph 要的 `PxScale`。
    ///
    /// ab_glyph 的 `PxScale` 基准不是 em，而是 `ascent - descent`（`Font::height_unscaled`），
    /// 雅黑那一档是 1320 而 unitsPerEm 是 1000——不换算的话所有文字会整体小 24%，
    /// 卡片版式全乱。换算式两边一约：`scale / height_unscaled == px / units_per_em`。
    fn scale(&self, px: f32) -> f32 {
        let em = self.font.units_per_em().unwrap_or(1000.0);
        if em == 0.0 {
            return px;
        }
        px * self.font.height_unscaled() / em
    }

    fn raster(&self, ch: char, px: f32) -> Cached {
        let key = (ch, px.to_bits());
        let mut cache = self
            .glyphs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = cache.map.get(&key) {
            let hit = Arc::clone(hit);
            cache.touch(key);
            return hit;
        }
        let done = Arc::new(uncached(&self.font, self.scale(px), ch));
        cache.put(key, Arc::clone(&done));
        done
    }
}

/// 真正解析一个字的轮廓并铺成覆盖率位图（缓存没命中时才走这里）
fn uncached(font: &ab_glyph::FontArc, scale: f32, ch: char) -> (GlyphMetrics, Vec<u8>) {
    let scaled = font.as_scaled(scale);
    let id = scaled.glyph_id(ch);
    let advance_width = scaled.h_advance(id);
    let glyph: Glyph = id.with_scale(scale);
    let Some(outline) = font.outline_glyph(glyph) else {
        return (
            GlyphMetrics {
                xmin: 0,
                ymin: 0,
                width: 0,
                height: 0,
                advance_width,
            },
            Vec::new(),
        );
    };
    // ab_glyph 的 `px_bounds` 是 **y 向下**的像素包围盒（min.y 是顶边），而画布那一路
    // 沿用 fontdue 的口径：`ymin` 是相对基线的**下**边界、向上为正。换算就一次取负。
    let bounds = outline.px_bounds();
    let (left, right) = (bounds.min.x.floor(), bounds.max.x.ceil());
    let (top, bottom) = (bounds.min.y.floor(), bounds.max.y.ceil());
    let width = (right - left).max(0.0) as usize;
    let height = (bottom - top).max(0.0) as usize;
    let mut coverage = vec![0u8; width * height];
    if width > 0 && height > 0 {
        outline.draw(|x, y, value| {
            let (x, y) = (x as usize, y as usize);
            if x < width && y < height {
                coverage[y * width + x] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
    }
    (
        GlyphMetrics {
            xmin: left as i32,
            ymin: -(bottom as i32),
            width,
            height,
            advance_width,
        },
        coverage,
    )
}

/// 一次加载好的常规 + 粗体两份字面
pub struct Typeface {
    regular: Face,
    bold: Option<Face>,
}

/// 缓存的键：`bili-image.font` 解析出来的（常规, 可选粗体）两把路径
type FaceKey = (PathBuf, Option<PathBuf>);

/// 按解析出的字体文件组合缓存字面，**只存 Weak**：谁在画谁拿着 Arc，最后一个 Arc 落下后
/// 这一档字面就整个还给分配器，下一轮真要画时再装载一次（0.01 s 量级，见 [`open`]）。
/// 惰性后端下这份常驻本来就只是字体文件的字节（思源宋体常规 11.1 MB + 粗体 11.5 MB）
/// 加一层有界字形缓存，不再是要长期按着的百 MB 轮廓——但"没人用就还"这条口径没变。
/// 键用解析后的文件路径而不是配置原文：`bili-image.font` 换了写法但指的是同一组文件时，
/// 不该再存第二份。
static CACHE: OnceLock<Mutex<HashMap<FaceKey, Weak<Typeface>>>> = OnceLock::new();

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
    /// `configured` 是 `bili-image.font` 的值：字体文件路径，或放在
    /// `config/bilibili/font/` 下的文件名（后缀可省），可用 `;` 再跟一个粗体。
    /// 留空则按系统候选路径找。找不到任何中文字体就报错，由调用方退回纯文本推送。
    ///
    /// 返回的一直是强引用：拿着它的那一段就保证字面不被换掉。同一档文件组合并发进来时只装载一次，
    /// 后到的排在锁上等同一份——字体文件那二十来 MB 字节不该在进程里存两份。
    pub fn discover(configured: &str) -> Result<Arc<Typeface>, String> {
        let key = resolve(configured.trim())?;
        let cache = CACHE.get_or_init(Default::default);
        let mut map = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(alive) = map.get(&key).and_then(Weak::upgrade) {
            return Ok(alive);
        }
        let face = Arc::new(load(&key)?);
        // 已经没人用的高级键就地清掉：这张表只留「现在正被拿着的」那几档
        map.retain(|_, weak| weak.strong_count() > 0);
        map.insert(key, Arc::downgrade(&face));
        Ok(face)
    }

    fn face(&self, bold: bool) -> &Face {
        if bold {
            self.bold.as_ref().unwrap_or(&self.regular)
        } else {
            &self.regular
        }
    }

    fn width_of(&self, text: &str, px: f32, bold: bool) -> f32 {
        let face = self.face(bold);
        text.chars().map(|ch| face.advance(ch, px)).sum()
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
        let reserve = face.advance('…', px);
        let mut out = String::new();
        let mut used = 0.0f32;
        for ch in flat.chars() {
            let width = face.advance(ch, px);
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
        if let Some(limit) = max_lines
            && rows.len() > limit
        {
            rows.truncate(limit);
            let reserve = face.advance('…', px);
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
        Flow { rows }
    }

    /// 画满这么多行要占多高（含末行的行距），排版时先量后画
    pub fn height(&self, flow: &Flow, leading: f32) -> f32 {
        flow.rows() as f32 * leading
    }

    /// 从 `first_baseline` 开始逐行画下去，返回下一段的基线位置
    // 一次排版调用要的就是这一整组：画布、断行结果、落点、字号、行距、表情表
    #[allow(clippy::too_many_arguments)]
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
    #[allow(clippy::too_many_arguments)]
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
    fn width(&self, face: &Face, px: f32, emojis: &EmojiSet) -> f32 {
        match &self.emoji {
            // 有图就按正方形一格占位，跟 draw 里贴图的口径一致
            Some(name) if emojis.contains(name) => px,
            _ => self.text.chars().map(|ch| face.advance(ch, px)).sum(),
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
        if ch == '['
            && let Some(end) = find_emoji(&chars, index)
        {
            if !chunk.is_empty() {
                out.push(word(std::mem::take(&mut chunk)));
            }
            out.push(emoji(chars[index..=end].iter().collect()));
            index = end + 1;
            continue;
        }
        if is_emoji_base(ch)
            && let Some(end) = find_emoji_code(&chars, index)
        {
            if !chunk.is_empty() {
                out.push(word(std::mem::take(&mut chunk)));
            }
            let run: Vec<char> = chars[index..=end].to_vec();
            out.push(emoji_image(run.iter().collect(), twemoji_code(&run)));
            index = end + 1;
            continue;
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

/// 一段该按表情图画的单元：`text` 留着原字符，图没下下来时照文字画
fn emoji_image(text: String, code: String) -> Unit {
    Unit {
        text,
        emoji: Some(code),
    }
}

/// 表情的起始码位：象形文字区、杂项符号与装饰符、星形符号，加上拼旗帜的区域指示符。
/// 汉字、假名与常见标点都不落在这些段里。
fn is_emoji_base(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF
    ) || is_regional(ch)
}

/// 只能跟在别的表情后面的修饰码位：变体选择符、零宽连接符、keycap 组合符、皮肤色调。
/// 少了这一层区分，连着两个 `❤️` 会被吃成一格。
fn is_emoji_extend(ch: char) -> bool {
    matches!(
        ch as u32,
        0xFE00..=0xFE0F | 0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF
    )
}

fn is_regional(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

/// 从 `start` 起能整段吃下的表情码位，返回结束下标（含）。
/// 只往修饰码位上续：零宽连接符后面还得是个表情才算数，两个各自成形的表情分开算两格。
fn find_emoji_code(chars: &[char], start: usize) -> Option<usize> {
    let head = chars.get(start).copied()?;
    if !is_emoji_base(head) {
        return None;
    }
    let mut end = start;
    let mut index = start + 1;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\u{200d}' {
            if chars.get(index + 1).copied().is_some_and(is_emoji_base) {
                end = index + 1;
                index += 2;
                continue;
            }
            break;
        }
        if !(is_emoji_extend(ch) || (is_regional(ch) && is_regional(chars[end]))) {
            break;
        }
        end = index;
        index += 1;
    }
    Some(end)
}

/// 一段表情码位对应的 twemoji 文件名（不含 `.png`）：十六进制小写、`-` 连接。
/// 末尾那个 `fe0f` 只在序列里没有零宽连接符时才去掉——照原插件那条规则来
/// （`DynamicModuleDraw.kt:443-452`）。
fn twemoji_code(chars: &[char]) -> String {
    let mut parts: Vec<String> = chars.iter().map(|ch| format!("{:x}", *ch as u32)).collect();
    if parts.last().map(String::as_str) == Some("fe0f") && !parts.iter().any(|part| part == "200d")
    {
        parts.pop();
    }
    parts.join("-")
}

/// 表情图去哪儿取：twemoji 的 72x72 png，与原插件同一个地址（`Api.kt:53`）。
/// 主字体里没有彩色 emoji 字形（fontdue 不认 CBDT），所以只能像原插件那样拉图。
pub(crate) fn twemoji_url(code: &str) -> String {
    format!("https://cdnjs.cloudflare.com/ajax/libs/twemoji/14.0.2/72x72/{code}.png")
}

/// 一段文本里出现了哪些 unicode 表情：`(twemoji 文件名, 下载地址)`，按出现顺序去重。
/// 下载侧拿它补进表情表，绘图侧靠同一个文件名命中。
pub(crate) fn twemoji_needs(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let Some(end) = find_emoji_code(&chars, index) else {
            index += 1;
            continue;
        };
        let code = twemoji_code(&chars[index..=end]);
        if !out.iter().any(|(have, _)| *have == code) {
            out.push((code.clone(), twemoji_url(&code)));
        }
        index = end + 1;
    }
    out
}

/// `[` 往后找到配对的 `]`，返回 `]` 的下标
fn find_emoji(chars: &[char], start: usize) -> Option<usize> {
    let tail = chars.get(start + 1..)?;
    let limit = tail.len().min(EMOJI_MAX_LEN);
    for (offset, ch) in tail.iter().take(limit).enumerate() {
        match ch {
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
    face: &Face,
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
        // fontdue 给的位图顶边在基线上方 ymax 处，而 Metrics 只有 ymin 与 height，
        // 于是 ymax = ymin + height - 1。写成 `baseline - height + ymin` 会把每个字形
        // 按它自己的下伸量挪开 (2*ymin - 1) 像素——汉字与数字的 ymin 不同，一行里就高低不齐。
        let top = baseline.round() as i32 - (metrics.ymin + metrics.height as i32 - 1);
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
/// 系统里的兜底候选：只在框架随包那份字体也拿不到时才用（安装目录缺 `fonts/`、
/// 或从源码直接跑）。正常装机路径见 [`resolve`] 里那句 `assets::cjk_font`。
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

/// 字面取哪一份，三级：`bili-image.font` 指定 > 框架随包那份（安装目录 `fonts/`）> 系统候选。
/// 粗体用 `;` 或 `,` 跟在后面，同样按这三条规则找。
///
/// 中间那级就是 `arona::runtime::assets::cjk_font()`：安装包把思源宋体简中释到运行目录，
/// GUI 与所有出图插件读的都是同一个出口，观感因此不随"这台机器装了什么字体"漂。
fn resolve(configured: &str) -> Result<(PathBuf, Option<PathBuf>), String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let (main, bold) = match configured.split_once([';', ',']) {
            Some((main, bold)) => (main.trim().to_string(), Some(bold.trim().to_string())),
            None => (configured.to_string(), None),
        };
        let regular = locate(&main).ok_or_else(|| {
            format!("配置里指定的字体不存在：{main}（写绝对路径，或把字体文件放进 config/bilibili/font/）")
        })?;
        let bold = match bold {
            Some(name) => {
                Some(locate(&name).ok_or_else(|| format!("配置里指定的粗体字体不存在：{name}"))?)
            }
            None => None,
        };
        return Ok((regular, bold));
    }
    if let Some(bundled) = arona::runtime::assets::cjk_font() {
        return Ok(bundled);
    }
    CANDIDATES
        .iter()
        .filter_map(|(regular, bold)| {
            let path = PathBuf::from(regular);
            path.is_file().then(|| (path, bold.map(PathBuf::from)))
        })
        .next()
        .ok_or_else(|| {
            "找不到中文字体：安装目录的 fonts/ 里没有字体文件，系统里也没命中候选。 \
             可在 config/bilibili/arona.yml 的 bili-image.font 指定字体文件路径"
                .to_string()
        })
}

/// 先当路径试，再当 `config/bilibili/font/` 下的文件名试（`.ttf/.otf/.ttc` 可省）
fn locate(name: &str) -> Option<PathBuf> {
    locate_in(name, &crate::config_dir().join("font"))
}

/// 按配置里那一个词找字体文件：能直接当路径用就用，否则到 `folder` 里按文件名找，
/// 后缀 `.ttf/.otf/.ttc` 可省（写了路径分隔符的只在 `folder` 内查最后那一段）。
fn locate_in(name: &str, folder: &Path) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_file() {
        return Some(direct);
    }
    let file = Path::new(name).file_name()?;
    for extension in ["", ".ttf", ".otf", ".ttc"] {
        let candidate = if extension.is_empty() {
            folder.join(file)
        } else {
            folder.join(format!("{}{extension}", file.to_string_lossy()))
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 按解析好的两把路径把字面读进内存。全量预解析字形轮廓要 0.3~0.6 秒，
/// 所以这条只在 [`CACHE`] 里那档没人拿着时才走（口径见那张表的说明）。
fn load(paths: &FaceKey) -> Result<Typeface, String> {
    let (regular, bold) = &paths;
    let regular = open(regular)?;
    let bold = bold
        .as_ref()
        .filter(|path| path.is_file())
        .map(|path| open(path))
        .transpose()?;
    Ok(Typeface { regular, bold })
}

/// 读一个字体文件成一份字面。`.ttc` 是字体集合，ab_glyph 取的是第 0 个 face。
///
/// 装载只是把文件字节收下来（思源宋体 11.1 MB，实测 0.01 s），字形要等真画到那个字才解析。
/// 这正是"用完就还"能成立的前提：字面不再是那份要 0.3~0.6 s 才解析得出来、
/// 一解析就常驻 160~250 MB 的重货，所以缓存换成 Weak 之后，重解析的代价可以忽略。
fn open(path: &Path) -> Result<Face, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("读字体 {} 失败：{err}", path.display()))?;
    match ab_glyph::FontArc::try_from_vec(bytes) {
        Ok(font) => Ok(Face {
            font,
            glyphs: Mutex::new(Glyphs {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }),
        Err(err) => Err(format!("字体 {} 解析失败：{err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bili-image.font` 只写文件名时，得能在 `config/bilibili/font/` 下命中（后缀可省）
    #[test]
    fn font_name_resolves_inside_the_config_font_folder() {
        let folder = std::env::temp_dir().join(format!("bili-font-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let real = folder.join("MyFont.ttf");
        std::fs::write(&real, b"placeholder").unwrap();

        assert_eq!(
            locate_in("MyFont", &folder).as_deref(),
            Some(real.as_path())
        );
        assert_eq!(
            locate_in("MyFont.ttf", &folder).as_deref(),
            Some(real.as_path())
        );
        assert_eq!(
            locate_in("sub/MyFont", &folder).as_deref(),
            Some(real.as_path()),
            "配置里手滑带上目录时，只认最后那一段，不去翻别处"
        );
        assert_eq!(locate_in("NoSuchFont", &folder), None);
        // 给了完整路径就走"当路径试"那一档，跟 folder 无关
        assert_eq!(
            locate_in(real.to_string_lossy().as_ref(), &folder).as_deref(),
            Some(real.as_path())
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn emoji_scan_pairs_only_the_nearest_bracket() {
        let nested: Vec<char> = "[a[b]".chars().collect();
        assert_eq!(find_emoji(&nested, 0), None);
        let ok: Vec<char> = "[笑]尾巴".chars().collect();
        assert_eq!(find_emoji(&ok, 0), Some(2));
    }

    #[test]
    fn unicode_emoji_codes_follow_the_twemoji_naming() {
        let heart: Vec<char> = "❤️".chars().collect();
        // 没有零宽连接符时末尾那个 fe0f 去掉：2764-fe0f → 2764
        assert_eq!(twemoji_code(&heart), "2764");
        // 美人鱼+女性：序列里有 200d，所以 fe0f 留着
        let mermaid: Vec<char> = "🧜‍♀️".chars().collect();
        assert_eq!(twemoji_code(&mermaid), "1f9dc-200d-2640-fe0f");
        assert!(twemoji_url("2764").ends_with("/72x72/2764.png"));
    }

    #[test]
    fn unicode_emoji_scan_skips_plain_text_and_dedupes() {
        let needs = twemoji_needs("这是🧜‍♀️只美人鱼，你敢和她直视10秒吗？❤️❤️");
        assert_eq!(needs.len(), 2, "重复的那个不该再要一遍：{needs:?}");
        assert_eq!(needs[0].0, "1f9dc-200d-2640-fe0f");
        assert_eq!(needs[1].0, "2764");
        // 汉字、假名、全角标点与方括号表情都不该被当成 unicode 表情
        assert!(twemoji_needs("本日の新活動【翻唱】[笑]").is_empty());
        assert!(twemoji_needs("100% & 3.14 ~ (´∀`)").is_empty());
    }

    #[test]
    fn two_adjacent_emoji_stay_two_units() {
        let pair = units("❤️❤️");
        assert_eq!(pair.len(), 2, "连着两个各自成形的表情该分开算");
        assert_eq!(pair[0].emoji.as_deref(), Some("2764"));
        // 旗帜是两个区域指示符拼出来的一格
        let flag = units("🇨🇳");
        assert_eq!(flag.len(), 1);
        assert_eq!(flag[0].emoji.as_deref(), Some("1f1e8-1f1f3"));
    }

    #[test]
    fn an_emoji_unit_keeps_its_text_for_the_fallback() {
        // 图没下下来时按原文字画：单元里得留着那个字符，宽度也照文字算
        let units = units("❤️");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "❤️");
        assert_eq!(units[0].emoji.as_deref(), Some("2764"));
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
        let widest = flow
            .rows
            .iter()
            .map(|row| row.iter().map(|token| token.width).sum::<f32>())
            .fold(0.0, f32::max);
        assert!(widest <= 200.0, "有一行顶破了 max_width：{}", widest);
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

    /// 字形缓存的两条边界：**不许越过上限**，且越过之后丢的必须是**最久没被用的**那一条。
    /// 前一版是"到顶整层作废"，多字号的场景会把热字反复清空，这条把它钉死。
    #[test]
    fn glyph_cache_stays_bounded_and_drops_the_stalest() {
        let face = Typeface::discover("").unwrap();
        let regular = face.face(false);
        let px = 32.0f32;
        // 私有区码位当夹具：不会与标点/数字撞上；那些字没有轮廓也没关系，
        // 缓存记的是"这个字算过了"
        let pool: Vec<char> = (0..(GLYPH_CACHE_MAX + 40) as u32)
            .filter_map(|at| char::from_u32(0xE000 + at))
            .collect();
        for ch in &pool[..GLYPH_CACHE_MAX] {
            regular.rasterize(*ch, px);
        }
        assert_eq!(
            regular.glyphs.lock().unwrap().map.len(),
            GLYPH_CACHE_MAX,
            "装满时该正好停在上限上"
        );
        // 碰一下最早那条，它就该从"最久没用的"变成"最近用过的"
        regular.rasterize(pool[0], px);
        for ch in &pool[GLYPH_CACHE_MAX..GLYPH_CACHE_MAX + 10] {
            regular.rasterize(*ch, px);
        }
        let cache = regular.glyphs.lock().unwrap();
        assert!(
            cache.map.len() <= GLYPH_CACHE_MAX,
            "越过了上限：{} > {}",
            cache.map.len(),
            GLYPH_CACHE_MAX
        );
        assert!(
            cache.map.contains_key(&(pool[0], px.to_bits())),
            "刚碰过的那条不该被淘汰"
        );
        assert!(
            !cache.map.contains_key(&(pool[1], px.to_bits())),
            "最久没用的那条该先走——这条红了就是 LRU 退成了 FIFO"
        );
        assert!(
            cache
                .map
                .contains_key(&(pool[GLYPH_CACHE_MAX - 1], px.to_bits())),
            "最后装进去的那批不该被整层丢掉"
        );
    }
}
