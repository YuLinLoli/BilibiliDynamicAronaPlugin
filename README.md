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
     plugins/bilibili/bilibili_dynamic_plugin.dll   ← 只改 plugin.yml 由框架自动生成
     config/bilibili/arona.yml                      ← 首次启动自动生成，改完保存即热重载
     data/bilibili/store.yml                        ← 订阅与水位（插件自己写）
     data/bilibili/image/<类别>/                     ← 图片缓存
   ```

4. 启动框架。日志里能看到装配行与四个定时任务（动态检测、直播检测、低频重排、缓存清理）。

**没有 B 站 Cookie 也能用**：匿名就能拉单人的动态流，订阅、绘图、推送、命令全都工作。
只有这几件事需要登录态，用 `/bili login` 扫码（Cookie 自动写回配置）：自动关注 UP、把 UP 归到 B 站分组、
开播检测（`check.liveInterval` 那一路）、`/bili new` 的时间线翻页。

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
| `command.names` | `["/bili","/b","/动态"]` | 主命令名与别名，必须带前缀 |
| `command.prefixFree` | `false` | 群聊里是否允许不带前缀直接发 |
| `account.cookie` | `""` | 完整 Cookie 串，`/bili login` 成功后自动写回 |
| `account.uid` | `0` | Cookie 所属 uid |
| `account.csrf` | `""` | `bili_jct`，留空则从 cookie 里解析 |
| `account.autoFollow` | `false` | 添加订阅时顺手关注这个 UP |
| `account.followGroup` | `""` | 自动关注时归到哪个 B 站分组（≤16 字符） |
| `check.interval` | `30` | 动态检测间隔(秒)，推荐 15-60 |
| `check.liveInterval` | `15` | 直播检测间隔(秒) |
| `check.lowSpeed` | `"3-8x2"` | 低频时段：3 点到 8 点把间隔乘以 2；留空全天同速 |
| `check.reportInterval` | `0` | 日志汇报间隔(分钟)，0 = 每次都报 |
| `check.timeout` | `20` | 单次 B 站请求超时(秒) |
| `push.messageInterval` | `500` | 同一个群连续多条之间的间隔(毫秒) |
| `push.pushInterval` | `1000` | 连续两个目标之间的间隔(毫秒) |
| `push.atAllPlus` | `SINGLE_MESSAGE` | @全体 单独一条；`PLUS_END` = 追加到最后一条后面 |
| `push.toShortLink` | `false` | 推送里的长链换成短链（依赖第三方，不推荐） |
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
| `cardOrnament` | `None` | 卡片装饰：`FanCard` / `QrCode` / `None` |
| `badgeEnable` | `true` | 卡片顶部标签 |
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

内置模板名：动态 `OnlyDraw` / `SimpleMsg` / `ForwardMsg` / `NoDraw`，
直播 `OnlyDraw` / `SimpleMsg` / `TextOnly`，直播结束 `SimpleMsg`。
`defaultDynamicPush` / `defaultLivePush` / `defaultLiveClose` 都指向 `SimpleMsg`。
表是自己加项的：键名就是 `/bili template` 里写的模板名。

占位符与原插件一致：

* 动态：`{draw}` 卡片、`{name}`、`{uid}`、`{did}` 动态 ID、`{type}`、`{time}`、`{content}` 正文、
  `{images}` 动态里的图、`{link}`、`{links}`
* 直播另有：`{rid}` `{title}` `{area}` `{cover}` `{startTime}` `{endTime}` `{duration}`
* `\n` 换行；`\r` 拆成多条消息；`{>>}` … `{<<}` 之间的内容发成合并转发

`forwardCard` 是 QQ 转发卡片的外观（`title` / `preview` / `summary` / `brief`），见「已知差别」第 6 条。

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

## 预留问题

功能面没有待接的项。翻译（第 27、28 条）在 2026-09-30 这一轮接上了：`translate.enable` 打开、
`translate.appId` / `translate.securityKey` 填好之后，检测任务在画动态卡之前调一次百度接口，
失败只记一条日志，不影响这条动态推不推。

剩下三条是**压测覆盖**没到，不是实现没做——它们都要先有 B 站登录态：

1. **动态卡与直播卡的出图**。未登录时动态/直播接口一律回 `code -101`，`/bili new` 停在拉取那一步，
   画卡与推图压根不会发生。扫过码之后发 `/bili new <UID>` 才算压到这一条链。
2. **百度翻译的真实调用**。它挂在动态拉取之后，而未登录时拉取就断了，所以本轮端到端**一次都没触发到**
   这一路（把 `translate.enable` 打开、填假密钥跑一轮，宿主日志里连「翻译」都不会出现）。目前只有
   `src/translate.rs` 那 10 项单测覆盖判定口径、签名拼法与响应解析；真实调用要登录态和真密钥同时齐。
3. **`login` 的成功分支**。拿到 Cookie 后 `entry.update` 会把它写回 `config/bilibili/arona.yml`，
   再看框架能不能原样读回（Cookie 串里有分号、空格、等号）。本轮压到的是失败分支。

## 端到端自测

假 OneBot 端与运行目录都在本机 `C:\Users\cheng\Arona-e2e\`：运行目录 `run-bili\` 里
`config/onebot.yml` 的 self_id 是 10000，`config/arona.yml` 的管理员写 100 与 12345，插件 dll 放
`plugins\bilibili\`。两个工装各自起一个 ws-forward 服务（默认 9001），一次只跑一个：

```
node fake-onebot-bili.mjs      # 命令序列：21 条本地 + 5 条联网
node fake-onebot-login.mjs     # /bili login 专测：出图 → 轮询 → 撤码
```

回话按「第 i 条命令下发到第 i+1 条之间收到的」归属，打到 `可疑 0 条` 才算过。宿主一律
`ARONA_NO_ELEVATE=1 ./arona-rs.exe --nogui` 起，跑完 `taskkill //IM arona-rs.exe //F` 收尾。

2026-09-30 实测：

- 命令序列 26 条回话，**可疑 0 条**，宿主日志 0 panic，插件握手「布局 5，契约 1.0.0」。
- `/bili login`：二维码由本地 `qr_png` 画成，以 `base64://…` 图片段发出——这是「插件绘图 → 框架发图」
  那条链第一次端到端压通；随后提示文案到，每 3 秒轮询一次 passport poll，约 3 分钟 B 站回 `86038`
  过期 → 撤掉那两条消息（`delete_msg` ×2）→ 回「登录失败 二维码已过期，重新发一次 login」。四条判定全过。
- `/bili new 501` 在未登录时回「获取动态失败 账号登录失效（code -101），请用 /bili login 重新扫码登录」，
  文案准确、不栈崩——这正是上面第 1、2 条压不到的直接原因。

## 开发

```
cargo fmt
cargo clippy --all-targets     # 目标：0 警告
cargo test --lib               # 136 项，2 项联网用例标了 #[ignore]
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
