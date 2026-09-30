//! 绘图：卡片画布、字体发现、动态卡与直播卡。
//!
//! 布局数值不写在代码里到处飞：四档画质各自一份字面值在 [`quality`]，颜色在 [`theme`]，
//! 两个模块的产物都由卡片构造函数按原插件的坐标规则取用。

pub(crate) mod canvas;
pub(crate) mod card;
pub(crate) mod live;
pub(crate) mod quality;
pub(crate) mod text;
pub(crate) mod theme;
