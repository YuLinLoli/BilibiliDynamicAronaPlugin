# bilibili-dynamic-arona-plugin

B 站动态 / 直播 / 番剧订阅推送插件，**Arona-rs 框架**的 dll 插件。

原型是 mirai 插件 [`Colter23/bilibili-dynamic-mirai-plugin`](https://github.com/Colter23/bilibili-dynamic-mirai-plugin)。
本仓库不是把 Kotlin 翻译过来，而是照它的**功能面**用 Rust 重写：订阅 UP 主动态与直播、追番、
类型/正则双过滤器、@全体、分组批量配置、模板推送、卡片绘图、扫码登录、链接解析、机翻补译文、图片缓存清理、
`/bili` 那一整棵子命令树。

一句话差别：**本插件不依赖任何前置插件**。原插件要 `chat-command` 才能解析命令、要 `mirai-skia-plugin`
才能绘图；这里命令走框架的分发器（自带中英别名与用法串），绘图用纯 Rust 的 `image` + `fontdue` 在插件里
画完，再把成品图交给框架发出去。配置里也没有「前置插件」这一栏——只有默认值，想改就改 yml。

## 安装

1. 先有 Arona-rs 框架（≥ 1.1.0）。
2. 构建：`cargo build --release`，产物 `target/release/bilibili_dynamic_plugin.dll`。
3. **框架停着的时候**把 dll 放进框架运行目录的 `plugins/bilibili/`：

   ```
   <框架运行目录>/
     plugins/bilibili/bilibili_dynamic_plugin.dll   ← 只放这一个 dll，plugin.yml 由框架装载时写出
     config/bilibili/arona.yml                      ← 首次启动自动生成，改完保存即热重载
     data/bilibili/store.yml                        ← 订阅与水位（插件自己写）
     data/bilibili/image/<类别>/                     ← 图片缓存
   ```

4. 启动框架。日志里能看到装配行与四个定时任务（动态检测、直播检测、低频重排、缓存清理）。

**没有 B 站 Cookie 也能用**（2026-09-30 实测）。插件启动后自己去 `x/frontend/finger/spi` 取一对匿名设备
指纹（`buvid3`/`buvid4`）带在每个请求上，实测未登录可拉：单人动态流（订阅检测的主路径）、UP 名片、
番剧详情、开播批量状态、直播间信息、单条动态。不带头像指纹时这些接口回的是 HTTP 412 与 `code -352 风控校验失败`，
看着像"要登录"，其实缺的是那个匿名凭证——`src/bili/api.rs` 的 `cookie()` 负责补它。

只有这几件事确实要登录态，用 `/bili login` 扫码（Cookie 自动写回配置）：关注时间线那一路
（有人订了"全部动态"才走，代码里已用 `bili.signed()` 拦住）、自动关注 UP、把 UP 归到 B 站分组、
`/bili video` 的关注直播列表。`bili.account.cookie` 留空即可，填了就用你那份。

> 名片接口偶发 `-352`：同一 IP 短时间连着打多个签名接口会被 B 站限流，隔一会儿自己就好，
> 不是 Cookie 的问题。把 `check.interval` 调大能缓解。

## 命令

主命令默认 `/bili`，别名 `/b`、`/动态`（`bili.command.names` 可改，**改这一栏要重启**，命令表在装配阶段定型）。
子命令共 33 条，别名不区分大小写，中文别名可直接照原插件 README 敲：

| 子命令 | 别名 | 用法 | 说明 |
| --- | --- | --- | --- |
| Help | `h` `help` `帮助` `menu` | | 看这份帮助 |
| Reload | `reload` `重载` | | 重新读一遍配置文件 |
| Color | `color` `颜色` | `<#rrggbb[;#rrggbb…]> [目标]` | 给某个目标单独指定卡片配色 |
| Add | `add` `follow` `添加` `订阅` | `<UP主UID\|番剧号> [目标]` | 订阅一个 UP 主，或追一部番 |
| Del | `del` `unfollow` `删除` `取消订阅` | `<UP主UID\|名字\|番剧号> [目标]` | 取消订阅 |
| DelAll | `delall` `删除全部订阅` | `[目标]` | 清空这个目标的全部订阅 |
| List | `list` `列表` | `[目标]` | 看这个目标订了些什么 |
| ListAll | `listall` `la` `全部订阅列表` | | 全局订阅总表（仅管理员） |
| ListUser | `listuser` `lu` `用户列表` | `[UP主UID\|名字]` | 看某个 UP 被哪些目标订着（仅管理员） |
| FilterMode | `filtermode` `fm` `过滤模式` | `<t\|r> <w\|b> [UID] [目标]` | 设类型/正则过滤器的黑白名单模式 |
| FilterType | `filtertype` `ft` `类型过滤` | `<动态\|转发动态\|视频\|音乐\|专栏\|直播> [UID] [目标]` | 按动态类型过滤 |
| FilterReg | `filterreg` `fr` `正则过滤` | `<正则> [UID] [目标]` | 按正文正则过滤 |
| FilterList | `filterlist` `fl` `过滤列表` | `[UID] [目标]` | 看过滤器 |
| FilterDel | `filterdel` `fd` `过滤删除` | `<t0\|r1…> [UID] [目标]` | 按下标删一条过滤器 |
| TemplateList | `templatelist` `tl` `模板列表` | `[d\|l\|le]` | 把每套模板各渲染一遍发成合并转发看效果 |
| Template | `template` `t` `模板` | `<d\|l\|le> <模板名> [目标]` | 给某个目标换模板 |
| Login | `login` `登录` | | 扫码登录 B 站账号，Cookie 写回配置（仅管理员） |
| AtAll | `atall` `aa` `at全体` | `[全部\|全部动态\|直播\|视频\|音乐\|专栏] [UID] [目标]` | 这类内容推送时 @全体 |
| DelAtAll | `delatall` `daa` `取消at全体` | `[种类] [UID] [目标]` | 取消 @全体 |
| ListAtAll | `listatall` `laa` `at全体列表` | `[UID] [目标]` | 看 @全体 设置 |
| Config | `config` `配置` | `[UID] [目标]` | 看这个目标当前生效的配置 |
| Search | `search` `s` `搜索` | `<动态ID>` | 按动态 ID 取一条画出来看 |
| Live | `live` `直播` | | 看当前在线人数最高的一间直播 |
| New | `new` `最新动态` | `<UID\|名字> [条数]` | 拉这个 UP 最新的几条动态 |
| Video | `video` `最新视频` | `<UID\|名字>` | 这个 UP 最新一条视频动态 |
| Create | `create` `创建分组` | `<分组名>` | 建一个分组（推送目标可以整批写） |
| ListGroup | `listgroup` `lg` `分组列表` | `[分组名]` | 看分组，或看某个分组的明细 |
| DelGroup | `delgroup` `dg` `删除分组` | `<分组名>` | 删分组（只有创建者能删） |
| AddGroupAdmin | `addgroupadmin` `aga` `添加分组管理员` | `<分组名> <QQ号,…>` | 授权别人也能替这个分组改订阅 |
| BanGroupAdmin | `bangroupadmin` `bga` `删除分组管理员` | `<分组名> <QQ号,…>` | 收回授权 |
| Push | `push` `添加分组` | `<分组名> <群号\|好友号,…>` | 往分组里加目标 |
| Ban | `ban` `移出分组` | `<分组名> <群号\|好友号,…>` | 从分组里删目标 |
| Clear | `clear` `清理失效目标` | `[确定]` | 清掉已经不在的群/好友的订阅（仅管理员） |

参数里那几个槽位的含义：

* **目标**：留空表示「当前这个会话」（群里发的就是本群，私聊就是本人）；写群号/QQ 号指定别处；
  写分组名就整批生效。`store.yml` 里存成 `g<群号>` / `f<QQ号>`，分组名不能全是数字。
* **UP 主**：数字当 UID 用；写名字则在本地名单里模糊找，命中多个会把候选都列出来让你选。
  `add 0` 是对「这个目标订的全部 UP」生效。
* **UID（过滤器那几条）**：留空表示对该目标的全部订阅都生效，写 UID 就只针对那一个 UP。
* **番剧号**：`ss123` / `md123` / `ep123` 都认，`md` 与 `ep` 会换算成 `ss` 再存。

分组是一条「可整批写的目标」：订阅关系记在分组自己那份上，后来才加进分组的成员也收得到；
成员自己单独订的东西照旧。`/bili push`/`ban` 改的是成员名单，不动订阅。

## 配置

`config/bilibili/arona.yml`，三块区，键名沿用原插件的 camelCase，注释由框架按字段生成，
改完保存即热重载（只有 `command.names` 要重启）。区块键带 `bili-` 前缀，避免和别家插件的重名。

### `bili` — 账号、节奏、推送、缓存、代理、翻译、链接解析

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `enable` | `true` | 总开关：关掉后检测任务不再推送，命令仍可用 |
| `admin` | `[]` | 框架管理员之外的额外管理员（QQ 号或群号） |
| `notifyEnable` | `true` | 非管理员动了订阅/分组/模板/过滤/@全体 时，给每个 `admin` 发一条私聊通报 |
| `showLoadingMessage` | `true` | `search`/`new`/`video` 这三条要联网的命令先回一句「加载中…」，结果出来后撤回 |
| `command.names` | `["/bili","/b","/动态"]` | 主命令名与别名，必须带前缀 |
| `command.prefixFree` | `false` | 群聊里是否允许不带前缀直接发 |
| `account.cookie` | `""` | 完整 Cookie 串，`/bili login` 成功后自动写回；同目录有 `cookies.json` 时以文件为准（见「导入浏览器 cookie」） |
| `account.uid` | `0` | Cookie 所属 uid |
| `account.csrf` | `""` | `bili_jct`，留空则从 cookie 里解析 |
| `account.autoFollow` | `false` | 添加订阅时顺手关注这个 UP |
| `account.followGroup` | `""` | 自动关注时归到哪个 B 站分组（≤16 字符） |
| `check.interval` | `30` | 动态检测间隔(秒)，推荐 15-60 |
| `check.liveInterval` | `15` | 直播检测间隔(秒) |
| `check.lowSpeed` | `"3-8x2"` | 低频时段：3 点到 8 点把间隔乘以 2；留空全天同速 |
| `check.reportInterval` | `0` | 接口失败时**每隔多少分钟**记一条日志，0 = 每次失败都记（节流的是上一条真正记出去的日志，不是上一次调用） |
| `check.timeout` | `20` | 单次 B 站请求超时(秒) |
| `push.messageInterval` | `500` | 同一个群连续多条之间的间隔(毫秒) |
| `push.pushInterval` | `1000` | 连续两个目标之间的间隔(毫秒) |
| `push.atAllPlus` | `SINGLE_MESSAGE` | @全体 单独一条；`PLUS_END` = 追加到最后一条后面 |
| `push.toShortLink` | `false` | 推送里的长链换成短链（依赖第三方，不推荐） |
| `push.liveCloseNotify` | `true` | 下播时播一条结束通知；关掉只记状态，不再向订阅目标播报 |
| `cache.downloadOriginal` | `false` | 是否下载动态里的原图 |
| `cache.expires` | `DRAW:7 IMAGES:30 EMOJI:90 USER:90 OTHER:30` | 各类图多少天没再被用到就清理，0 = 不清 |
| `proxy` | `[]` | 代理列表（见「已知差别」第 9 条） |
| `translate.enable` | `false` | 机翻开关：打开且密钥齐了才调百度接口 |
| `translate.appId` | `""` | 百度翻译开放平台的 APP ID |
| `translate.securityKey` | `""` | 百度翻译的密钥，与 APP ID 成对；留空则不调接口 |
| `translate.cutLine` | `\n\n〓〓〓 翻译 〓〓〓\n` | 正文与译文之间的分割线 |
| `linkResolve.enable` | `true` | 链接解析开关 |
| `linkResolve.trigger` | `at` | `at` = 只有 @机器人 的消息才试，`always` = 见消息就试，`never` = 关 |
| `linkResolve.returnLink` | `false` | 出图之后是否再补一条原始链接 |
| `linkResolve.patterns` | `[]` | 自定义识别式（捕获组一是 id），留空用内置七条 |
| `linkResolve.allow` | `[]` | 允许解析的域名白名单，留空用内置那份 |
| `linkResolve.reject` | `[]` | 额外禁止的域名 |

### `bili-image` — 卡片外观

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `quality` | `1000w` | 宽度档位：`800w` / `1000w` / `1200w` / `1500w` |
| `theme` | `v3` | 绘图主题：`v3` / `v3RainbowOutline` / `v2` |
| `font` | `""` | 字体文件**绝对路径**，或放在 `config/bilibili/font/` 下的**文件名**（后缀 `.ttf/.otf/.ttc` 可省）；留空自动找系统中文字体。后可跟 `;` 指定粗体 |
| `defaultColor` | `#d3edfa` | 默认主题色，多个用 `;` 分隔构成渐变；单个值按 colorGenerator 自动生成 |
| `cardOrnament` | `None` | 卡片装饰：`QrCode` / `None`；`FanCard` 这一档当前不出效果（见「已知差别」第 29 条） |
| `drawEnable` | `true` | 绘图总开关：关掉之后卡片不画，模板里的 `{draw}` 落文字占位 |
| `badgeEnable.left` | `true` | 卡片顶部左标签（头像那枚） |
| `badgeEnable.right` | `false` | 卡片顶部右标签（配色条那枚）；两档都关掉时整条标签带的高度一起收掉 |
| `colorGenerator.hueStep` | `30` | 渐变色生成（仅当主题色只给一个值时生效） |
| `colorGenerator.lockSB` | `true` | 锁定饱和度与亮度 |
| `colorGenerator.saturation` | `0.25` | 锁定的饱和度 0.0-1.0 |
| `colorGenerator.brightness` | `1.0` | 锁定的亮度 0.0-1.0 |
| `footer.dynamicFooter` | `""` | 动态页脚模板，支持 `{name} {uid} {id} {type} {time}`；留空不画页脚 |
| `footer.liveFooter` | `""` | 直播页脚模板 |
| `footer.footerAlign` | `CENTER` | 页脚对齐：`LEFT` / `CENTER` / `RIGHT` |

像素布局内置在 `draw/` 里，这一区只暴露原插件中用户真会改的那几项。
单个订阅想要别的配色，用 `/bili color` 单独设，不必动这里。

### `bili-template` — 推送模板

内置模板名：动态 `OnlyDraw` / `SimpleMsg` / `ForwardMsg` / `NoDraw`，另附原插件那五档
`DrawOnly` / `TextOnly` / `OneMsg` / `TwoMsg` / `ForwardMsg`；直播 `OnlyDraw` / `SimpleMsg` / `TextOnly`，
另附 `DrawOnly` / `OneMsg` / `TwoMsg`；直播结束 `SimpleMsg` / `ComplexMsg`。
两边同名的档位内容一致，改名那一档见「已知差别」第 37 条。
`defaultDynamicPush` / `defaultLivePush` / `defaultLiveClose` 都指向 `SimpleMsg`。
表是自己加项的：键名就是 `/bili template` 里写的模板名。

占位符与原插件一致：

* 动态：`{draw}` 卡片、`{name}`、`{uid}`、`{did}` 动态 ID、`{type}`、`{time}`、`{content}` 正文、
  `{images}` 动态里的图、`{link}`、`{links}`
* 直播另有：`{rid}` `{title}` `{area}` `{cover}` `{startTime}` `{endTime}` `{duration}`
* `\n` 换行；`\r` 拆成多条消息；`{>>}` … `{<<}` 之间的内容发成合并转发

`forwardCard` 是 QQ 转发卡片的外观（`title` / `preview` / `summary` / `brief`），见「已知差别」第 6 条。

`drawEnable: false` 时卡片不画，`{draw}` 那格会落成一句占位文本
（动态 `[绘制动态失败]`、直播 `[绘制直播图片失败]`）——这与原插件关掉 `drawEnable` 后的行为一致。
只想要卡片不要文字，把模板换成 `OnlyDraw`；想要文字不要卡片，换成 `NoDraw`。

### 导入浏览器 cookie（`cookies.json`）

不想在 QQ 里扫码，可以把浏览器里已登录的 B 站 cookie 导出来用：装 EditThisCookie 之类的扩展，
在 `bilibili.com` 域下导出 JSON，存成 `data/bilibili/cookies.json`，重启插件即可。

* 只认数组里的 `SESSDATA` 与 `bili_jct` 两项，**缺一不算登录态**；两项都空值也不算。
* **文件优先**：文件里拼得出登录态时覆盖 `bili.account.cookie`；拼不出来（文件坏、缺项）时
  回退用配置里那份，并往宿主日志记一条告警。
* 同一份坏文件只告一次：插件每轮检测都会重读这个文件，按文件修改时间去重，改过一次文件才会再提醒。
* 热重载配置或重启之后会重读，`/bili login` 成功则直接写配置那份 cookie，两者互不干扰。

## 已知差别

这些是刻意与原插件不同的地方，以及受框架/接口所限做不了的部分。

1. **帮助是文本，不是 `HELP.png`**。原插件发一张图片版帮助；这里直接回一份文本清单，条目与上表同源，
   改了子命令表帮助文本自动跟着变。
2. **`/bili color` 按目标存，不按 UID 存**。原插件那张 `uid → 颜色` 的表在多群共用时会互相踩；这里颜色挂在
   具体目标（群/好友/分组）上，各群互不影响。
3. **`/bili clear` 要两步确认**。第一步只列出「这些目标已经不在 bot 的群/好友列表里」，带 `确定` 再执行才真删。
4. **`/bili video` 只有「最新一条视频动态」**。原插件那路 `searchUserVideo` 接口需要登录态且已不稳定，
   这里改从动态流里挑第一条视频类型的动态。
5. **群权限只在命令层查**。原插件靠 mirai 的对象模型随时能读群成员角色；这里在命中命令时读一次
   （框架的 `Bot::groups()` / 群成员缓存），不在钩子里为每条消息查群。
6. **合并转发的卡片只有 `title` 通道生效**。框架发的 `send_group_forward_msg` 只带一个标题字段，
   `preview` / `summary` / `brief` 三项配置留着但当前 OneBot 实现取不到处安放。
7. **`{images}` 实现为图片段**。原插件把它展开成图片消息段；这里一致，但一张图都没有时该位置直接留空，
   不会退化成一段占位文字。
8. **没有 `enableConfig.showLoadingMessage`**。原插件「处理中先回一句」依赖 mirai 的事件拦截时序；
   框架的命令分发是「回话恰好一次」，加这句会变成两条消息，所以不做。
9. **`proxy` 当前不生效**。出站 HTTP 走框架的代发出口（`arona::runtime::http`），那个出口没有代理参数；
   配置项留着是为了和原插件对齐，想让流量走代理请在框架/系统层配 `HTTP(S)_PROXY`。
10. **短链只还原一层**。框架的 HTTP 出口只回响应正文，没有状态码也没有 302 的 `Location`，
    所以 `b23.tv` 这类短链只能试一次跳转，取不到就当解析失败，不会连环追下去。
11. **`/bili live` 用 `GetWebList` 挑在线人数最高的一间**，接口不可用时退回订阅目标自己的 `live_status`。
12. **`/bili config` 是一份静态清单**，列出该目标当前生效的各项；原插件那套「按编号改配置」的向导不做，
    改配置请直接编辑 `arona.yml`（保存即热重载，还有 GUI 可填）。
13. **`/bili templatelist` 会把一层转发摊平**发出合并转发看效果；模板节点本身不带时间戳，所以列表里的
    `{time}` 用的是当前时间。
14. **改名要重启**。`command.names` 之外的所有配置都是热重载的。
15. **`prefixFree` 想生效，还要框架侧 `commandPrefixes` 为空**——那是框架的配置，不是本插件能决定的。
    默认关着：群聊里要 @机器人 或带 `/`。
16. **UP 改名要重启才会同步**到 `store.yml` 里那份名字表（检测路径不反查名字，避免每轮打接口）。
17. **过滤器/`@全体`/模板/`color` 对分组写时，是按分组「当前成员」批量落**。之后新加进分组的成员不会自动继承
    这些设置，但订阅关系（记在分组自己那份上）会自动生效。
18. **`add 0` 表示「对该目标已订的全部 UP」**，与原插件一致；`del 0` 同理。
19. **原插件 `delFilter` 的下标越界问题在这里不会复现**：越界直接回「没有这条过滤器」，不会把整个列表删空。
20. **`/bili addgroupadmin` 要求被授权对象是 bot 的好友**（框架侧才认得出这个人），
    **`/bili push` 要求 bot 真的在那个群里**——否则回话里会直说原因，不会静默失败。
21. **视频/专栏没有「通用小卡」版式**：原插件对这两种附属卡片有单独的小图排版，这里统一按附件区绘制
    （封面 + 标题 + UP + 时长/字数），版式随 `quality` 档位缩放。
22. **退群后自动清订阅走的是 notice 事件**（`GroupDecrease` 且 `user_id == self_id`），依赖框架正常投递 notice。
23. **表情图有独立缓存桶 `EMOJI`**：正文里 `[笑]` 这类原文会在推送前下载好并直接画进卡片，
    所以 `cache.expires.EMOJI` 是真生效的。原插件的表情没有这一路。
    `OTHER` 这一路本插件没有对应素材（番剧封面、直播封面都归 `IMAGES`），配置项留着只为键名对齐，
    清不到东西也不报错。
24. **字体两种写法都认**（`bili-image.font`）：绝对路径直接打开；只写文件名（后缀可省）则到
    `config/bilibili/font/` 下找。这一目录不存在时会自动按系统候选字体走。
25. **分组名可以带空格**：`create` / `listgroup` / `delgroup` 把剩余参数整体当分组名，
    与原插件 `chat-command` 的单个 `String` 参数行为一致。
26. **不解析「互动数 / 置顶 / 仅粉丝可见」这三族字段**：原插件的模板与绘图里都没有它们的功能面，
    带着只会让 `store.yml` 变胖。B 站挡掉正文时（屏蔽、仅粉丝、已删除），卡片上照抄 B 站给的那句提示。
27. **译文只进卡片，不进文本模板**：与原插件一致——那条 `trans(text)` 只喂给动态卡绘图，
    `{content}` 从来不带译文。开了 `NoDraw` 那一档就等于没有翻译。
28. **翻译走 https**：原插件的地址是 `http://api.fanyi.baidu.com/…`，而 APP ID 与签名就挂在查询串里，
    明文跑公网等于摊给沿途每一跳看。这里用百度同一套服务的 `https://fanyi-api.baidu.com/…`。
    判定「该不该译」与原插件同口径：先摘掉 `[笑]` 这类中括号，含假名就译，整篇没有汉字也译。
29. **卡片装饰只认 `QrCode`**：`FanCard`（粉丝牌）在原插件是画在作者行上的勋章装饰
    （`DynamicModuleDraw.kt:542` 那两档），本插件没铺 B 站的勋章数据，填了等同 `None`；键留着只为对齐配置面。
30. **「加载中…」占位与操作通知都在（这两档先前记的是"没做"，2026-09-30 深夜补上）**：
    `bili.showLoadingMessage` 与 `bili.notifyEnable` 对应原插件 `EnableConfig` 的同名两项，
    具体口径与和原插件的差别见下面第 39、40 条。
31. **画质四档内置，字体不自动下载**：原插件允许 `customOverload` 逐档覆盖像素布局（`DynamicImageQuality.kt:14`），
    这里四档写死；原插件找不到字体会自动下一份默认字体（`Init.kt:77`），这里找不到就记一条日志、这条只发文本。
32. **单 bot**：原插件的 Napcat 多 bot 探测（`NapcatCheckTasker.kt`）没有对应项——一个框架实例对应的就是它自己那条连接。
    原插件那两个分档开关在这里各有归宿：`liveCloseNotifyEnable` 就是 `push.liveCloseNotify`（关掉下播只记状态、
    不再向订阅目标播报）；`cacheClearEnable` 不单开开关，把 `cache.expires` 各类都填 `0` 就是不删。
33. **`muted`（静音）是个只读字段**：`store.yml` 里有它，`/bili config` 会打「静音：是/否」，推送路径上三处都会
    跳过静音目标，但**没有任何命令或配置项能把它打开**（原插件没有这一功能面，`mute` 那几处命中全是 `Mutex`）。
    留着的代价是清单上有个改不动的开关，撤掉的代价是将来想做要重写读侧——这条留给你定。

34. **番剧的基线存的是剧集号（`new_ep.id`），不是集名文本**：这条推翻了我先前在此处的说法
    （曾写成「有些番 B 站就是不填集名」）。2026-09-30 连抓三部（`ss38942` 已完结、`ss298872` 连载中、
    `ss319354` 已完结）**一个都没有 `newest` 键**——那是早年接口的形状，现在的最新集在 `new_ep` 那一格
    （`{"desc":"连载中","id":6291966,"is_new":1,"title":"9"}`，集号还是个光秃秃的「9」），
    集名只在 `episodes` 数组对应那一项的 `long_title` 里。所以卡片文案由这两处拼成「第9话 发烧不出门」，
    而比对认 `id`：号单调唯一，集名会撞车（同一话的中文配音版号不同、名子一样）。
    拿文本比对的坏处本轮真吃到了——空串跟基线一比就判成「换集」，推出去一条尾巴空着的「更新了：」。
    读不到 `new_ep.id`（给 0）时这一条本轮不作数：不推，也不动基线。

35. **B 站 JSON 里的 `null` 一律当「没这个键」**：动态流（`feed/space`）那份包把 `major` 的每一族兄弟键
    都占着位置填 `null`（`{"archive":null,"article":null,"live":null,"live_rcmd":{…},…}`），而单条动态
    （`detail`）那份只带真内容有的那一族。按「键在不在」分支的解析在动态流上会**全线走错支**——
    实测因此把直播推荐动态的房间信息整块丢掉，渲染侧再拿默认值画出一张「正在直播：（空）/ 房间号：0」的卡
    （2026-09-30 端到端真收到过一条）。`bili/model.rs` 的 `at()` 现在把 null 折成「没有」，
    两份包对同一条动态给出同样的解析结果，这一点由两条 fixture 测试分别钉住。

36. **名片与主播名走匿名接口，不走签名那条**：UP 主资料用 `x/web-interface/card`（昵称、签名、等级都真给，
    实测 Lv6），直播主播名与头像用 `live_user/v1/Master/info`。原插件用的 `x/space/wbi/acc/info` 要 WBI 签名，
    实测**同一秒连打两次就回 `-352 风控校验失败`**（响应只有 120 字节），未登录时等级还回 Lv0。
    链接解析里 `t.bilibili.com/<动态ID>` 那一族仍必须签名（B 站只有那条给单条动态），所以连点会吃到 -352，
    插件如实回一句「把检测间隔调大一些再试」，不静默、不假装成功。
37. **模板名与原插件一档不改名，另外多三档**：原插件 `TemplateConfig` 那几档（动态
    `DrawOnly`/`TextOnly`/`OneMsg`/`TwoMsg`/`ForwardMsg`、直播 `DrawOnly`/`TextOnly`/`OneMsg`/`TwoMsg`、
    结束 `SimpleMsg`/`ComplexMsg`，`BiliConfig.kt:227-247`）名字与模板串都照搬，那边配置里写
    `defaultDynamicPush: OneMsg` 的换过来不用改。这边另有 `OnlyDraw` / `SimpleMsg` / `NoDraw` 三档，
    其中 `SimpleMsg` 用 `\r` 把卡片、正文、配图拆成三条，是默认的推送形态。
    **配置文件是按当前代码生成的**：旧那份 yml 的 `dynamicPush` 表里没有新补的那几档，把默认指过去会查不到
    而回退到 `SimpleMsg`——删掉 `config/bilibili/arona.yml` 让框架重生成就行（实测重生成出来九档齐全）。
38. **几处默认值刻意保守**：`account.autoFollow` 原 `true` → `false`（订阅不擅自替你关注）、
    `account.followGroup` 原 `"Bot关注"` → 留空（不预设分组名）、`cache.downloadOriginal` 原 `true` → `false`
    （默认不落地原图，附件仍照发）、`cardOrnament` 原 `FanCard` → `None`（见第 29 条）、
    `push.messageInterval`/`pushInterval` 从 100/500 毫秒放宽到 500/1000（连打会被 B 站判 -352）、
    `check.interval` 从 15 秒放宽到 30、`check.timeout` 从 10 秒放宽到 20、`footerAlign` 从 LEFT 改 CENTER、
    `defaultDynamicPush` 从 `OneMsg` 改成 `SimpleMsg`。
39. **操作通报（`bili.notifyEnable`）落在管理员的会话上**：非管理员动了订阅/分组/模板/过滤/@全体，
    插件给 `bili.admin` 里每个号发一条「操作人/位置/操作/结果」的私聊。两个口径与原插件不同：
    * 原插件把通报发给 `findContactAll(BiliConfig.admin)`——那只会命中机器人**已经认识的会话**；
      这边也是：先在订阅表里认这个号出现过的是群还是好友，两边都没出现过就按私聊试一次。
    * 通报比群里那句回话**先发出去**（它在命令返回之前跑）。实测两条只差 2 毫秒，看不出顺序。
    * 「结果」那一行只取回复里的文本段并截到 120 字；卡片图与合并转发进不了这一行，
      那种命令自己就把东西发走了，通报里写「（结果由命令自己发出）」。
40. **「加载中…」只挂 `search`/`new`/`video` 这三条**：原插件那一档（`showLoadingMessage`）还挂在
    `templateList` 上（`DynamicCommand.kt:224`），因为那边预览要画图。这边模板预览用假数据、
    既不联网也不绘图，占位一条只会闪一下，所以没挂。撤回走框架的 `Bot::recall`，
    占位号取的是自己那条发送回执的 `message_id`。

## 预留问题

功能面没有待接的项（第 33 条那个改不动的 `muted` 字段除外——它不是原插件的功能面，是我自己多加的，去留待你定）。
翻译（第 27、28 条）在 2026-09-30 这一轮接上了：`translate.enable` 打开、
`translate.appId` / `translate.securityKey` 填好之后，检测任务在画动态卡之前调一次百度接口，
失败只记一条日志，不影响这条动态推不推。

下面第 1、2 条是**压测覆盖**没到，不是实现没做——而且没有一条还需要 B 站登录态（这一句此前写错过，见下面「端到端自测」里 2026-09-30 的那一轮）；第 3 条是一个等你拍的取舍，插件不替你决定：

1. **百度翻译的真实调用**。它挂在动态拉取之后，现在拉取通了，缺的只是一对真密钥：
   `translate.enable` 打开、`appId` / `securityKey` 填好才会出门。目前只有 `src/translate.rs`
   那 10 项单测覆盖判定口径、签名拼法与响应解析。
2. **`login` 的成功分支**。拿到 Cookie 后 `entry.update` 会把它写回 `config/bilibili/arona.yml`，
   再看框架能不能原样读回（Cookie 串里有分号、空格、等号）。这条只能你扫码，本轮压到的是失败分支。
3. **作为附件直发的图片段是什么编码，取决于图床那一次给的是哪一份**。`cache.downloadOriginal=false`（默认）时
   插件先要 `…@720w.webp`、失败才回退原图，而附件发的是拿到的原始字节，所以实测两种都出现过：
   番剧/直播封面那次是 WebP（52 KB，`base64://UklGR…` 开头），`/bili new` 那轮是 JPEG（`base64:///9j/…` 开头）。
   画进卡片的那些不受影响——插件自己出的是 PNG。要不要在发送前统一转一次归你定：
   转一次体积会涨一个量级，不转则收端认不认得看运气。

原来列在这里的第 1 条「**直播卡被真实开播数据驱动**」已经在 2026-09-30 晚间这一轮压到了：
`feed/space` 里那条直播推荐动态真推出了一张 PNG 直播卡 + 「老番茄 正在直播：最手忙脚乱的一集 /
分区：其他单机 / 房间号：21852」，直播检测那一轮也真推过「3号直播间 正在直播：哔哩哔哩音悦台」。
代价是当场挖出第 35 条那个 `null` 占位的解析缺陷——它原本是画一张空卡（房间号 0、主播名与标题都空）。

## 端到端自测

假 OneBot 端与运行目录都在本机 `C:\Users\cheng\Arona-e2e\`：运行目录 `run-bili\` 里
`config/onebot.yml` 的 self_id 是 10000，`config/arona.yml` 的管理员写 100 与 12345，插件 dll 放
`plugins\bilibili\`。除了 `peek-images.mjs` 每个工装都起一个 ws-forward 服务（默认 9001），一次只跑一个：

```
node fake-onebot-bili.mjs      # 命令序列：21 条本地 + 5 条联网
node fake-onebot-login.mjs     # /bili login 专测：出图 → 轮询 → 撤码
node fake-onebot-link.mjs      # 链接解析钩子：七条带 at 段的消息，含一条负例
node fake-onebot-season.mjs    # 番剧跟跑：订一部番后静默四轮，看检测有没有每轮都查
node fake-onebot-new.mjs       # 软限流探针：同一个 uid 连发 N 次 /bili new，数几次出真动态
node fake-onebot-listen.mjs    # 纯监听：不下发命令，只数定时推送与其中的图片段
node fake-onebot-cookie.mjs    # 只发 `/bili config`，把回话全文打出来（查配置项有没有真生效）
node fake-onebot-notify.mjs    # 操作通报与「加载中…」：发命令的人刻意用 20002（不是框架管理员）
node peek-images.mjs           # 不是服务端：把 sent log 里的图片段解成「PNG/JPEG 宽x高 字节数」
```

`peek-images.mjs` 是判「绘图开关有没有生效」的那把尺：图片段正文里 `iVBORw0KGgo` 开头是插件画的
PNG 卡片，`/9j/4AAQSkZ` 开头是动态附带的 JPEG 原图——只看有没有 `iVBOR` 就够了，比数条数可靠。

回话按「第 i 条命令下发到第 i+1 条之间收到的」归属，打到 `可疑 0 条` 才算过。宿主一律
`ARONA_NO_ELEVATE=1 ./arona-rs.exe --nogui` 起，跑完 `taskkill //IM arona-rs.exe //F` 收尾。
`fake-onebot-link.mjs` 现在不是定长间隔下发，而是「发一条、等它的回话停住再发下一条」——
定长 5 秒压不住 B 站的抖动（签名接口吃到 -352 时那一条要磨十几秒），迟到回话会掉进下一条的桶里，
把「负例该安静」冤枉成骚扰。2026-09-30 晚间改完之后负例那条才第一次判对。

2026-09-30 实测：

- 命令序列 26 条回话，**可疑 0 条**，宿主日志 0 panic，插件握手「布局 5，契约 1.0.0」。
- `/bili login`：二维码由本地 `qr_png` 画成，以 `base64://…` 图片段发出——这是「插件绘图 → 框架发图」
  那条链第一次端到端压通；随后提示文案到，每 3 秒轮询一次 passport poll，约 3 分钟 B 站回 `86038`
  过期 → 撤掉那两条消息（`delete_msg` ×2）→ 回「登录失败 二维码已过期，重新发一次 login」。四条判定全过。
- `/bili new 501` 在未登录时回「获取动态失败 账号登录失效（code -101），请用 /bili login 重新扫码登录」，
  文案准确、不栈崩。换成一个真实存在的 uid（546195）之后同一条命令匿名可拉，所以上面第 1、2 条压不到的
  原因是**缺真密钥与缺人扫码**，不是缺登录态——这一句在早些版本里写反过。
- 链接解析钩子（六条带 at 段的消息）：**空间卡、番剧卡、直播卡三条都真出了图片段**，视频那条按「没铺详情接口」
  回三行文本，没有链接的负例**安静**（没骚扰）。空间那条先前写的「解析失败 code -101」是插件自己的问题，
  不是 B 站要登录：`user()` 当时走的是签名的 `x/space/wbi/acc/info`。换成匿名的 `x/web-interface/card` 之后
  回的是「UP主：老番茄（UID 546195）／签名：…／等级：Lv6／直播间：https://live.bilibili.com/21852」＋一张卡。
  这一条链不需要 B 站登录，一次验到「入站消息段对钩子可见 → at 判定 → 识别式命中 → 带 Referer 抓图 → 图片段发出」五跳。
- 直播卡**两次被真实数据驱动**：`https://live.bilibili.com/23058` 那条走房间详情＋主播资料，回
  「3号直播间 正在直播：哔哩哔哩音悦台／分区：唱见电台／房间号：23058」；`https://t.bilibili.com/1253801885478944772`
  那条是**直播推荐动态**（房间信息裹在 `live_rcmd.content` 里，且这个 content 不给主播名与头像），回
  「老番茄 正在直播：最手忙脚乱的一集／分区：其他单机／房间号：21852」——缺的那两格由动态作者顶上
  （`tasks::live_of`）。同一动态随后也被定时检测推了一次，附带的图片段是一张 PNG 直播卡。
  修通之前它推的是一张「 正在直播：」空卡，根因见上面第 35 条。
- `t.bilibili.com/<动态ID>` 这一族在连打时会被 B 站判风控（`-352`），插件回「解析失败 被 B 站风控拦了
  （code -352: 风控校验失败），把检测间隔调大一些再试」——这句话本轮实测出现过，属实数据不是编的。
- 直播状态**批量**接口这一路本轮也匿名抓到了真包（`room/v1/Room/get_status_info_by_uids`，
  POST `{"uids":[546195]}`）：回执是 `data.item_by_uid` 直接以 uid 为键、**没有** `room` 那一层嵌套，
  字段给的是 `room_id`/`uname`/`face`/`area_name`/`cover_from_user`/`keyframe`/`live_status`/`online`/`live_time`。
  `parse_live_status` 逐条对得上，两处先前只在猜的地方就此定死：头像认 `face`（真给，不需要拿
  `Master/info` 补，更不能拿 `user_cover` 顶），封面先取 `cover_from_user`、主播没设自定义封面才退 `keyframe`。
- `feed/space` 有**软限流**：本轮多次出现「code 0、`items` 是空数组」，宿主日志里 HTTP 全 200，
  定时检测因此一整轮 0 推送。这不是插件坏了，压测时别拿「0 推送」当失败判据——要看 `store.yml` 里水位有没有动。
  同一现象在命令面上也出现过：改完第 34 条之后复跑全命令序列，26 条回话仍然**可疑 0 条**、宿主 0 panic，
  但 `/bili new 546195` 那轮回的是「未找到动态」，而更早两轮同一条命令回的是真动态＋图片段。
  为这条专门起了探针（`fake-onebot-new.mjs`：同一个 uid 连发四次，上一条回话一停就发下一条，
  实际打出去是 12:20:15 / 17 / 19 / 21 四拍，`NEW_RC=0`）——
  **四次里两次出真动态（PNG 卡片＋JPEG 配图两段图片段）、两次「未找到动态」**，宿主日志四条
  `feed/space?…host_mid=546195…w_rid=…` 全部发出、全部正常返回。同一个人六秒之内不可能「没了动态又有了」，
  所以这是接口在同一个窗口里忽空忽有，不是解析回归——探针只证明了「同一条命令能连出两次真动态」，
  至于是按连接、按节点还是按配额抽样给空数组，B 站没说，我也不猜。`new()`（`commands/browse.rs:410`）没有水位去重，
  空数组只能来自接口。结论落在插件行为上：**它分不清「真没动态」与「被软限流挡了」**，照 B 站给的
  `items` 空数组回「未找到动态」。要不要拿 `psource`/水位把这两种分开是你的取舍，不是缺陷清单。
- 删掉 `config/bilibili/arona.yml` 让框架重生成，`linkResolve.enable` 出来是 `true`，跟
  `LinkResolveConfig::default()`（`config.rs:321`）一致——先前运行目录里那份 `false` 是手工关的，不是默认值。
- `drawEnable` 两档都实压过：关掉那一轮假端收到 37 条推送、11 个图片段**全是 JPEG**（`/9j/4AAQSkZ` 开头，
  也就是动态自带的原图），`iVBOR` 一张都没有——卡片确实没画；打开那一轮出 12 张 1000w PNG 卡片。
  `{draw}` 那格落的是 `[绘制动态失败]` / `[绘制直播图片失败]` 两句文案，与原插件同分支同文案（`SendTasker.kt:327/432`），
  不是这里画不出来。判开关只看图片段开头，别数条数。
- `badgeEnable` 拆成 `left` / `right` 两档之后按卡片高度判定：两档都关掉时 13 张卡每张**恰好矮 45 px**
  （那 45 px 就是这一档的 `badge_height`），只关一档时高度不变——右档顶到标签带起点，带子该多高还多高。
  单测 `turning_both_badges_off_shrinks_the_card` 钉的是同一件事。
- `cookies.json` 走的是**文件优先**，正反各压一轮（判据不用 `/bili config` 的「登录态」那格——它打的是
  `BiliConfig.account.signed()`，看的是配置串，文件里的 cookie 根本不经它）：
  配置里 `account.cookie` 留空，`subs` 加上通配档 `0`（`wants_all()` 为真），而「关注时间线」这一路
  只在 `bili.signed()` 时才发（`tasks/mod.rs:253`），所以**宿主日志里有没有 `feed/all` 代发**就是硬判据。
  放一份 EditThisCookie 形状的完整假 cookie（`SESSDATA` + `bili_jct`，另带一项干扰用的 `buvid3`）→
  每轮真发 `feed/all`，110 秒窗口里 8 次，且 0 条 cookie 告警；假 cookie 被 B 站回
  `code -400: 请求错误`，插件报「关注时间线读不出来：B 站返回 code -400: 请求错误」，不栈崩。
  把文件移走复跑同一份订阅 → `feed/all` **一次都不发**，同窗口 `feed/space` 9 次照旧（其它路径没坏）。
- 坏文件只告一次：放一份只给 `SESSDATA`、缺 `bili_jct` 的文件（`cookie_from_editthis` 判它不算登录态），
  90 秒 / 约 6 轮检测里宿主日志恰好 **1 条**告警——「cookies.json 里缺少 SESSDATA 或 bili_jct，这一轮仍用配置里的
  cookie」（21:30:28 那一条），同窗口 `feed/all` 0 次、`feed/space` 6 次。先前是每轮刷 3 条，
  现在按文件 mtime 去重（`api.rs` 的 `warn_cookie`），改过一次文件才会再提醒。
- `push.liveCloseNotify` 双向实压：把 `store.yml` 的 `live.'546195'` 伪造成 `status: 1 / closed: false`
  （真人在 21:02 已下播，所以这一轮必然判定「结束」）。开关关掉时假端**0 条推送**，而 `store.yml` 被插件写成
  `status: 0 / closed: true`——状态照记，只是不播；改回 `true` 复跑同一份伪造状态，逼出 1 条完整结束通知：
  「老番茄 的直播已结束／标题：最手忙脚乱的一集／分区：其他单机／房间号：21852／开播：2026年09月30日 17:41:38／
  结束：2026年09月30日 21:39:58／时长：3:58:20」。这是这一族通知第二次被真数据驱动。
- 原插件那几档模板名补齐之后实压过一遍：删掉 `config/bilibili/arona.yml` 让框架按新代码重生成，`dynamicPush`
  表里 `DrawOnly`/`TextOnly`/`OneMsg`/`TwoMsg` 四档都出现了（旧那份 yml 里确实没有，所以先前把
  `defaultDynamicPush` 指到 `OneMsg` 会查不到而回退成 `SimpleMsg`——F 轮就是这么撞上的，不是渲染 bug）。
  重生成之后配 `defaultDynamicPush: OneMsg` + `drawEnable: false` 逼一轮，12 条推送**每条恰好三行**：
  「[绘制动态失败]⏎老番茄@视频⏎https://t.bilibili.com/1253703543616962617」，`{type}` 真给出「视频／转发动态」，
  带图 0 条——`OneMsg` 本来就不含 `{images}`，与原插件那串一模一样。

同一天还修了一处真缺陷：**番剧检测原先只在启动/热重载时跑一次**（`schedule()` 里一次性 `spawn_as`，注释却
写着「跟着动态那一轮跑」），追番的新集永远不会推。现在挂在 `dynamic_job` 里跟动态同轮，`run_seasons` 自带的
`SEASON_BUSY` 互斥让重叠那一次直接跳过。

这一条**不需要 B 站登录就能实压**（`pgc/view/web/season` 匿名可查），验法见 `fake-onebot-season.mjs` 与
`fake-onebot-listen.mjs`：订一部番之后什么都不发，宿主日志里每轮都真出现 `[BilibiliDynamic:番剧检测]`
（本轮 140 秒里 20 次请求 = 5 轮 × 3 部），静默期推 0 条——首轮只记基线，正确。
第 34 条修完之后拿 `fake-onebot-season.mjs` 复跑过一遍（`SEASON2B_RC=0`）：`/bili add ss38942 20001`
回「追番成功( •̀ ω •́ )✧ [爆笑罗米熊与丹米兔]」，`/bili list` 里番剧那一栏有它，静默 4 轮检测共推 0 条，
`store.yml` 落成的基线是**新格式**（剧集号写成字符串）：`seasons: { ss38942: '418481' }`。

但晚间复跑时先暴露了一件事：**早些时候那句「把基线改成假的→认出新集→真推一条」压的其实是第 34 条那个缺陷**——
那轮用的 `ss38942` 根本没有 `newest` 键，推出去的文案是「更新了：」，集名那一格是空的（本轮用同一手法
——把基线改成假的——在 `sent-listen2.log` 里又原样复现了一次），我当时只数了「有没有推」，没看推的文案。
按第 34 条改完解析之后重做一轮，`store.yml` 里一次预置三种基线同跑，三条走向各自都对：

- `ss298872`（连载中）配**旧剧集号** `6291908` → 判成换集，推出一条**带真集名**的更新：
  「更新了：第9话 发烧不出门」＋一张 52 KB 的封面图（`base64://UklGR…`，WebP），基线改写成 `6291966`。
  番剧跟跑这条链到这一刻才算第一次推对东西：「检测任务 → 基线比对 → 素材下载 → 模板填充 → 节流 → 框架发图」。
  这条推送是「模板文本 + 下载来的封面原图」两段，中间那张图**不是画出来的卡**——番剧没有对应的卡片版式，
  `tasks/mod.rs` 里 `draw_missing: String::new()` 那句注释说得明白，`{draw}` 换成空串、模板首条因此被丢掉。
- `ss319354`（已完结）配**老格式的文本基线**「第01集（占位旧基线）」→ 认不出是号，只重记成 `6427585`、**不推**
  ——升级基线格式不该骚扰群。
- `ss38942` 配**与接口同号**的 `418481` → 判成没换集，基线一个字没动、不推。

`season_move` 里「接口没给 `new_ep.id` 就这一轮不作数」那一支本轮**没有真实样本**（三部都给了号），
只有单测钉着；它是给预告片期、接口回 `null` 那类留的兜底。

同一轮里还顺手修掉一处：`ep` 号那一路的 `season_by_ep` 忘了像 `season()` 那样剥 `result`，
而这份包顶层没有 `season_id`，所以**按 `ep` 号订阅或解析番剧链接必定报「B 站没给剧集 X 所属的番剧」**
（实测 `?ep_id=6291908` 的顶层键只有 `code,message,result`）。`ss` 那一路一直是通的，所以先前没暴露。
修完给 `fake-onebot-link.mjs` 加上这条 ep 用例再跑七条：**共 8 条回话，可疑 0 条**（`LINK7_RC=0`）、
宿主 0 panic，ep 那条回的是「番剧：小车队大救援：自然护卫队／最新：第9话 发烧不出门／
播放：https://www.bilibili.com/bangumi/play/ss298872」＋一张 PNG 卡，`ss38942` 那条也多出了「最新：第13话」
这一行——这一行在先前那版里因为取不到值，被 `if !season.newest_title.trim().is_empty()` 整条跳掉了。

还有一条记在这儿当观测：**作为附件直发的那一份图，编码是跟着图床那一次给的是哪一份走的，插件不重编码**。
`fetch`（`tasks/mod.rs:873`）在 `cache.downloadOriginal=false`（默认）时先要 `…@720w.webp`，那一次失败才回退原图；
拿到的原始字节直接塞进图片段，解码只为画图。所以同一个附件位子上实测出现过两种编码：
定时推送那次是 WebP（`base64://UklGR…`，52 KB），`/bili new` 这轮是 JPEG（`base64:///9j/4AAQSkZJRg…`）。
链接解析与卡片那几段不受影响——那是插件画完再发的 PNG。
要不要在发送前统一转成 PNG/JPEG 归你定：插件已经引了 `image`，`fetch` 里那句 `decoded.to_rgba8()` 手边就有像素，
真要转只是把 `raw` 换成重编码那几行；但这会改变附件字节与缓存那份的对应关系（缓存存原始字节），
属于取舍不是缺陷，所以我停在这儿等你拍。

2026-09-30 深夜这一轮补的是最后两档配置（`notifyEnable`、`showLoadingMessage`），工装换成
`fake-onebot-notify.mjs`——**发命令的人刻意用 20002**，因为框架 `managers` 是 `[100, 12345]`，
用老脚本那个 100 会被当成管理员，通报这条路根本不会走。两轮：

- 两个开关都开：`/bili new 546195 1` 先出一条 `send_group_msg`「加载中...」（`message_id` 1002），
  结果那几条发完后紧跟一条 `delete_msg {"message_id":1002}`；`/bili add 546195` 之后多出一条
  `send_private_msg -> 12345`，正文是「bilibili 操作通报⏎操作人: 外人（20002）⏎位置: 群 20001⏎
  操作: /bili add 546195⏎结果: 之前订阅过这个人哦」。`/bili config` 的回显里那行变成
  「操作通报 开｜加载中占位 开」。脚本判定 `loading=1 / delete_msg=1 / 通报=1`，退出码 0。
- 两个开关都关：同一个脚本连跑三条命令，出站只有 3 条 `send_group_msg`，
  `send_private_msg` 0 条、`delete_msg` 0 条、占位 0 条——负例干净，说明这两档是真 gate 不是装饰。
  这一轮 `/bili new` 回的是「未找到动态」（B 站那一路的抖动，先前 `fake-onebot-new.mjs` 那轮数过），
  与开关无关。

## 功能面差集收口

判"改完了"用的是三面差集，每一面都比到具体条目，不是比总数：

- **命令面**：`DynamicCommand.kt` 有 33 个 `@SubCommand`，本插件 [`SUBS`](src/commands/mod.rs) 33 行，
  规范名逐条对齐（把驼峰统一成小写再比，diff 为空）；别名表除了照抄的那批还补了中文写法，
  比对走 `to_lowercase`，所以原插件的 `delAll` / `listAll` / `filterType` 这些驼峰写法在这边同样命中。
  不依赖 chat-command：命令名、别名、参数位次都在这份表里自己判。
- **配置面**：`BiliConfig.kt` 的 `EnableConfig` 8 个开关逐个有下落——`drawEnable`（`bili-image` 那区）、
  `liveCloseNotifyEnable`、`translateEnable`、`notifyEnable`、`showLoadingMessage` 是真开关，
  `lowSpeedEnable` / `cacheClearEnable` / `proxyEnable` 在这边改成"留空即不开"（`check.lowSpeed` 空串、
  `cache.expires` 全 0、`proxy` 空表），语义等价而少一个键。`CheckConfig` / `PushConfig` /
  `TemplateConfig` / `CacheConfig` / `LinkResolveConfig` 的键逐条过，`TemplateConfig` 那几档模板名
  一档不改名（第 37 条）。
- **任务与类型面**：Kotlin 那 11 个 tasker 的归属——`DynamicCheck`/`LiveCheck`/`LiveCloseCheck` 进
  `tasks`，`SendTasker` 的节流与 @全体 拼法进 `push`，`CacheClearTasker` 进 24 小时一轮的清理，
  `ListenerTasker` 的链接解析进 `link.rs` 钩子、它那条 `BotLeaveEvent → removeAllSubscribe`
  对应 `left_group`（`link.rs:319`，带 `user_id == bot.self_id()` 守卫，缺它会让任意成员退群清空整群订阅）；
  `BiliTasker`/`BiliCheckTasker` 是协程基类与低频/汇报横切，框架的任务出口顶上；
  `NapcatCheckTasker` 是 NapCat 那套在线状态轮询的旁路，连接归框架管，故意不移植。
  `DynamicFilterType` 六项与 `AtAllType` 六项与 `/bili ft`、`/bili aa` 的参数表逐项一致。


## 开发

```
cargo fmt
cargo clippy --all-targets     # 目标：0 警告
cargo test --lib               # 156 项（154 通过 + 2 项联网用例标了 #[ignore]）
cargo build --release
```

工具链由 `rust-toolchain.toml` 钉死（只写 channel，不写 components——写进去会让每次 cargo 都去补装组件）。
`.cargo/config.toml` 给 msvc 目标加了 `-C target-feature=+crt-static`，这样 dll 不拖 VCRUNTIME。

框架依赖现在是绝对路径（插件仓在 `G:`、框架仓在 `C:`，相对路径写不成）：

```toml
arona = { package = "arona-rs", path = "C:/Users/cheng/RustroverProjects/Arona-rs/crates/arona", version = "1.1.0", default-features = false }
```

发版时换成 git 标签或 crates.io 那一行（见 `Cargo.toml` 注释）。`default-features = false` 是硬要求：
GUI 由宿主决定，插件带上 `gui` 会把 `eframe`/`wgpu` 整条链拖进 dll。

本 crate 没有 `main`，也不能独立运行——OneBot 连接、运行时、配置读写、发消息全部用框架的预制出口。
`crate-type = ["cdylib", "rlib"]`：`cdylib` 产出装载用的 dll，`rlib` 那份只给 `cargo test` 用，宿主永远不链接它。
