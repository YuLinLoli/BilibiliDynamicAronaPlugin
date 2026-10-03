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
指纹（`buvid3`/`buvid4`）带在每个请求上，实测未登录可拉：单人动态流（**没登录时订阅检测就走这一条**）、UP 名片、
番剧详情、开播批量状态、直播间信息、单条动态。不带头像指纹时这些接口回的是 HTTP 412 与 `code -352 风控校验失败`，
看着像"要登录"，其实缺的是那个匿名凭证——`src/bili/api.rs` 的 `cookie()` 负责补它。

**有登录态时订阅检测换成关注时间线**（2026-10-03，就是「预留 5」那条按 A 做完的结果）：一轮只打 `feed/all`
一到几页（翻页上限是 `check.timelinePages`，翻够了提前停），一页把所有人的新动态一起带回来，再按本地订阅表分桶，
所以**一轮的请求数由翻页上限决定，跟订阅了多少人无关**——这是 `firehose_round` 里那句
`for page in 1..=timeline_pages` 的循环结构；端到端只钉得到「每一轮都先打关注流」（那轮 7 次 `feed/all` 对 7 轮），
真账号下拿到数据的那一半还欠着，见预留 5。逐 UP 那一条留在没登录时用，`bili.signed()` 就是那道口；
关注流连着三轮读不出来也会临时退回逐 UP，免得一份坏 Cookie 把整个检测弄停。
剩下几件事仍然只有登录才做：自动关注 UP、把 UP 归到 B 站分组、`/bili video` 的关注直播列表。
`bili.account.cookie` 留空即可，填了就用你那份。

> 名片接口偶发 `-352`：同一 IP 短时间连着打多个签名接口会被 B 站限流，隔一会儿自己就好，
> 不是 Cookie 的问题。把 `check.interval` 调大能缓解。

## 命令

主命令默认 `/bili`，别名 `/b`、`/动态`（`bili.command.names` 可改，**改这一栏要重启**，命令表在装配阶段定型）。
子命令共 35 条（原插件那 33 条逐个对齐，另加本插件自己的 `mute` / `unmute`），别名不区分大小写，中文别名可直接照原插件 README 敲。
下面这张表不是手抄的摆设：`cargo test` 里有一条单测把它读进来与真正生效的 `SUBS` 逐行比名字、比别名集合、比用法串，
两个方向都查，对不上就红（详见「功能面差集收口」）：

| 子命令 | 别名 | 用法 | 说明 |
| --- | --- | --- | --- |
| Help | `h` `help` `帮助` `menu` | | 看这份帮助 |
| Reload | `reload` `重载` | | 重新读一遍配置文件 |
| Color | `color` `颜色` | `<#rrggbb[;#rrggbb…]> [目标]` | 给某个目标单独指定卡片配色 |
| Mute | `mute` `silence` `静音` `暂停推送` | `[目标]` | 让这个目标暂停收推送，订阅关系一条都不动 |
| Unmute | `unmute` `resume` `取消静音` `恢复推送` | `[目标]` | 恢复这个目标的推送 |
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
| Config | `config` `配置` | `[UID] [目标]` | 开交互式配置：回编号往下选（At全体 / 主题色 / 模板 / 过滤器），回 `全局` 看完整清单 |
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
| Clear | `clear` `清理失效目标` | | 删掉已经不在的群/好友（连订阅带配色整条删）；列完清单回一句 `确定` 才真删（仅管理员） |

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
| `account.autoFollow` | `true` | 添加订阅时顺手关注这个 UP。检测走关注时间线，那条只覆盖**已关注**的人，所以这条关掉又没手动关注过的订阅会一条都收不到（原插件也是 `true`） |
| `account.followGroup` | `""` | 自动关注时归到哪个 B 站分组（≤16 字符） |
| `check.interval` | `30` | 动态检测间隔(秒)，推荐 15-60 |
| `check.liveInterval` | `15` | 直播检测间隔(秒) |
| `check.timelinePages` | `3` | 一轮最多翻几页关注时间线（一页约二十条，翻页不签名）。这一页最旧一条已经比所有水位都新时才继续翻，所以平静时段实际只打一次 |
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
| `colorGenerator.lockSB` | `true` | 锁定饱和度与亮度。全篇只有这一个键带连续大写——原插件自己就写成 `lockSB`（`BiliConfig.kt:181`），字段上单独 `rename` 点名，不让 `rename_all` 把它改成 `lockSb` |
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
3. **`/bili clear` 与原插件同形**：先列出「这些目标已经不在 bot 的群/好友列表里」，然后等一句答复——
   回 `确定` 才真删，回其它任何一句都算 `已取消`，五分钟没人回就静默作废（`DynamicCommand.kt:452-464`）。
   那句答复的作用域是**发命令那个人在这个会话里的下一句话**——另一个群、或者同一个群里另一个人说
   `确定` 都不算，与原插件 `fromEvent.nextMessage` 同一档（键是 `wizard::key` 那个「会话#发起人」，
   只按会话存的话，同群里旁人一句话就能把别人的清单顶掉）。
   先前这里写成两步命令（`clear` 再 `clear 确定`），理由是「框架没有 `nextMessage` 这种出口」，
   那个判断是错的：消息钩子加插件自己那份会话表就能做同一件事，跟第 12 条那个向导是同一套料。
   确定之后删的是**整条目标**（`store::Data::remove_targets`：订阅、配色、模板覆盖、分组挂名一起没了），
   不只清内容。这份清单的候选是拿 `targets` 的键扫出来的，只清内容的话同一个死群每轮都重新上榜——
   第一版就是这么漏的，`fake-onebot-clear.mjs` 最后一步压出来的。原插件不复现，因为它遍历的是
   `dynamic[uid].contacts`，把 contact 从每个 uid 的名单里摘掉就没有残留；这里目标键是主键，就得删条目。
4. **`/bili video` 只有「最新一条视频动态」**。原插件那路 `searchUserVideo` 接口需要登录态且已不稳定，
   这里改从动态流里挑第一条视频类型的动态。
5. **群权限只在命令层查**。原插件靠 mirai 的对象模型随时能读群成员角色；这里在命中命令时读一次
   （框架的 `Bot::groups()` / 群成员缓存），不在钩子里为每条消息查群。
6. **合并转发的卡片只有 `title` 通道生效**。框架发的 `send_group_forward_msg` 只带一个标题字段，
   `preview` / `summary` / `brief` 三项配置留着但当前 OneBot 实现取不到处安放。
7. **`{images}` 实现为图片段**。原插件把它展开成图片消息段；这里一致，但一张图都没有时该位置直接留空，
   不会退化成一段占位文字。
8. **「加载中」的实现路径不同（这一档先前记的是"不做"，2026-09-30 深夜做了，见第 30、40 条）**：
   原插件靠 mirai 的事件拦截时序在命令执行前插一句；框架的命令分发是「回话恰好一次」，
   照搬会变成两条消息留在群里。这边的办法是发出去再收回来——占位拿自己那条发送回执的
   `message_id`，结果发完后调框架的 `Bot::recall` 把它撤掉，群里最终仍只看到结果那几条。
9. **`proxy` 当前不生效**。出站 HTTP 走框架的代发出口（`arona::runtime::http`），那个出口没有代理参数；
   配置项留着是为了和原插件对齐，想让流量走代理请在框架/系统层配 `HTTP(S)_PROXY`。
10. **短链只还原一层**。框架的 HTTP 出口只回响应正文，没有状态码也没有 302 的 `Location`，
    所以 `b23.tv` 这类短链只能试一次跳转，取不到就当解析失败，不会连环追下去。
11. **`/bili live` 用 `GetWebList` 挑在线人数最高的一间**，接口不可用时退回订阅目标自己的 `live_status`。
12. **`/bili config` 是交互式向导，不是那份静态清单**（2026-10-01 改的）。原插件那句「回复编号往下选」
    靠 mirai 的 `whileSelectMessages`（`service/ConfigService.kt`）——协程 + 等这个会话的下一条消息。
    框架没有那一句，但给了做成同一件事的两块料：消息钩子（`HookFlow::Handled` 能在命令分发之前把这条
    消息吃掉）与插件自己存的会话表，所以这里的循环是**每条入站消息推进一步**的状态机。2 分钟没人回作废、
    连着两次认错收摊，都是原插件那两个数。每一步都不自己写库，而是把用户回的话拼成一条 `/bili` 子命令
    交给命令层跑——权限门控、拒绝文案、操作通报因此与手打命令完全同源。原先那份一次性清单没删，
    挂在向导里的 `全局` 那一档后面。四处与原插件不同：
    * 只有**发起人**回的消息算输入。原插件在群里听任何人的发言，别人随手打一个「2」
      就能把管理员正在配的那一格改掉。
    * 每一步做完**重打整张菜单**，中括号里的当前值跟着刷新。原插件只补一句「输入编号以继续」
      并引用那张旧菜单——上面的值已经是错的了。
    * 编号补齐。原插件 At全体 那一档把「添加」和「删除」都印成 `2`（`ConfigService.kt:46-47`），
      却按 `3` 处理删除，照着屏幕敲的人必然撞一次「没有这个选项哦」。
    * 改的是这个目标在 `store.yml` 里的覆盖项（配色、模板、过滤器、@全体），`arona.yml` 那一区仍是全局默认，
      改它保存即热重载；向导里重打菜单用的值就是这两层合起来的结果。
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
    所以 `cache.expires.EMOJI` 是真生效的。原插件同样有这一档（`BiliConfig.kt:273` 的
    `CacheType.EMOJI to 7`）——这一句先前写的是「原插件的表情没有这一路」，2026-10-01 对着
    Kotlin 复核样式预览时改了过来。
    同一轮补上另一半：unicode 表情（❤️、🧜‍♀️ 那种）主字体画不出来，照原插件没有彩色
    emoji 字体时的办法处理，按 twemoji 的 png 补同一格，细节见第 42 条。
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
33. **`muted`（静音）这一档本插件有、原插件没有**：`store.yml` 里那个字段的读侧从一开始就在——`全局` 那一档打「静音 是/否」，
    检测与推送路径上三处会跳过静音目标（`tasks/mod.rs` 两处、`push/mod.rs:170`）。原先它**没有任何写入口**，
    所以清单上挂着一个改不动的开关（原插件确实没这一功能面，Kotlin 那几处 `mute` 命中全是 `Mutex`）。
    2026-10-01 补成 `/bili mute` / `/bili unmute`（别名 `silence`/`resume`/`静音`/`取消静音`/`暂停推送`/`恢复推送`），
    按目标整体停推复推：只翻那一格，订阅、配色、模板、过滤器一概不动，`unmute` 之后原样恢复。
    权限走 `Call::targets()` 那道现成的门——当前会话谁都能动，替别的目标动就要管理员身份或分组归属，
    非管理员被拒同样进操作通报。这是本插件命令面比原插件多出的两条（35 对 33）。

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
38. **几处默认值刻意保守**：`account.followGroup` 原 `"Bot关注"` → 留空（不预设分组名）、
    `cache.downloadOriginal` 原 `true` → `false`
    （默认不落地原图，附件仍照发）、`cardOrnament` 原 `FanCard` → `None`（见第 29 条）、
    `push.messageInterval`/`pushInterval` 从 100/500 毫秒放宽到 500/1000（连打会被 B 站判 -352）、
    `check.interval` 从 15 秒放宽到 30、`check.timeout` 从 10 秒放宽到 20、`footerAlign` 从 LEFT 改 CENTER、
    `defaultDynamicPush` 与 `defaultLivePush` 从 `OneMsg` 改成 `SimpleMsg`。
    `account.autoFollow` 曾经也在这一列里（`true` → `false`，理由是「订阅不擅自替你关注」），
    2026-10-03 检测主路径换成关注时间线之后就改回原插件那个 `true` 了：那条只覆盖这个账号**关注过**的人，
    订阅却不关注就成了「订了收不到」。
    2026-10-01 拿 `BiliConfig.kt` 的默认值又逐键比了一遍，下面这五处是当时漏记在这儿同款改动的：
    * `check.lowSpeed` 原 `"0-0x2"` → `"3-8x2"`。原默认那串首尾都是 0，`low_speed_window`（`config.rs:241`）
      判 `from == to` 就回 `None`——也就是说原插件默认压根不降速，它的 `lowSpeedEnable=true` 其实是空转；
      这边给的是一个真会生效的凌晨窗口。
    * `push.atAllPlus` 原 `PLUS_END` → `SINGLE_MESSAGE`。拼在最后一条尾部那种，碰上末条是合并转发会被
      静默跳过（`push/mod.rs:191` 那个 `appendable` 特判，@全体 就这么丢了），单独先发一条不会。
    * `admin` 原是一个 `Long`（`0` 表示没有）→ 这边是列表：能同时写多个 QQ 号与群号，也能一个都不写。
    * 百度那两把密钥从 `baidu.APP_ID` / `baidu.SECURITY_KEY` 挪成 `translate.appId` / `translate.securityKey`
      ——少一层子块，键名也不再是全大写蛇形。
    * `forwardCard` 四串默认文案重写：原 `{name} {type} 详情` / `时间: {time}\n{content}` / `ID: {did}` /
      `[{name} {type}]` → 这边 `{name} 的动态` / `{content}` / `点击查看完整动态` / `{name}: {content}`。
      按第 6 条当前只有 `title` 那格真发得出去，其余三串留着只为对齐配置面。
39. **操作通报（`bili.notifyEnable`）落在管理员的会话上**：非管理员动了订阅/分组/模板/过滤/@全体，
    插件给 `bili.admin` 里每个号发一条「操作人/位置/操作/结果」的私聊。两个口径与原插件不同：
    * 原插件把通报发给 `findContactAll(BiliConfig.admin)`——那只会命中机器人**已经认识的会话**；
      这边也是：先在订阅表里认这个号出现过的是群还是好友，两边都没出现过就按私聊试一次。
    * 通报比群里那句回话**先发出去**（它在命令返回之前跑）。实测两条只差 2 毫秒，看不出顺序。
    * **被拒的操作也会通报**：发不发只看子命令在不在改动状态的那张白名单里，不看命令成没成。
      2026-10-01 中午拿非管理员（20002）连压两支，各中一类：参数写错（「格式错误，请输入16进制颜色，
      如: #d3edfa」）与身份不够（「Bot不为管理员, 无法使用At全体」），两句都原样进了「结果」那一行。
      管理员正好该看到这类尝试，所以没按成功与否筛。
    * 「结果」那一行只取回复里的文本段并截到 120 字；卡片图与合并转发进不了这一行，
      那种命令自己就把东西发走了，通报里写「（结果由命令自己发出）」。
40. **「加载中…」只挂 `search`/`new`/`video` 这三条**：原插件那一档（`showLoadingMessage`）还挂在
    `templateList` 上（`DynamicCommand.kt:224`），因为那边预览要画图。这边模板预览用假数据、
    既不联网也不绘图，占位一条只会闪一下，所以没挂。撤回走框架的 `Bot::recall`，
    占位号取的是自己那条发送回执的 `message_id`。
41. **一轮动态检测的接口选型：登录之后与原插件同一条路，未登录才不同**。原插件每轮只打一次关注流
    （`DynamicCheckTasker.kt:38-39` → `x/polymer/web-dynamic/v1/feed/all`），一页约二十条把所有人的新动态
    一起带回来，再按本地订阅表过滤作者 mid（同文件 `:41`、`:53`）。2026-10-03 起这边有登录态时也是这一条
    （`tasks/mod.rs` 的 `firehose_round`），另加两处与原插件不同：一是**翻页**——原插件只看首页，一页装不下
    就会被顶出去那条永久漏掉，这边按 `check.timelinePages` 往上翻、直到这一页最旧一条压到所有水位之下才停
    （`should_fetch_more`）；二是**未登录留了逐 UP 那条匿名路**（`per_up_round`），因为匿名 `feed/all` 恒 `-101`
    而匿名 `feed/space` 还出内容。「订阅越多、一轮越慢」这一档差异在登录后已经没有了；取舍与实测见「预留问题」第 5 条。
    还有第三处是那个匿名接口带的：`feed/space` 正是**含置顶动态**那一个（原插件 `Dynamic.kt:24` 的 `hasTop` 挑的就是它，
    `false` 时它拿的是 `feed/all?host_mid=`），置顶那条排在页首、id 却比近期的小，所以逐 UP 这一路的水位只能按 id
    取最大、不能按页序取——2026-10-03 深夜那个「第一次订阅之后整页历史重推」就是没取最大留下的，见「预留问题」第 7 条。
42. **unicode 表情按 twemoji 的 png 补，因此多一个第三方出站域名**：地址写死成
    `https://cdnjs.cloudflare.com/ajax/libs/twemoji/14.0.2/72x72/<码位>.png`，与原插件同一档
    （`Api.kt:53`）。码位规则照原插件抄（`DynamicModuleDraw.kt:443-452`）：十六进制小写、`-` 连接，
    末尾那个 `fe0f` 只在序列里没有零宽连接符时才去掉；皮肤色调、旗帜各自合成一格，
    连着两个 ❤️ 不并成一格（这条是单测逼出来的，第一版就是错在无限往后吃码位）。
    因为出站多了一个非 B 站域名，`bili/api.rs` 的 `headers` 同时收了一次口：Cookie 只发给
    `bilibili.com`/`biligame.com`/`hdslb.com`，不再跟着请求去第三方——收口之前 `bytes()` 对任何
    URL 都附同一份头，直接加 twemoji 就会把 `SESSDATA` 送给 cdnjs。图下不来时不拦推送，
    那一格照原文字画，还是读得懂。

## 预留问题

功能面没有待接的项（原先挂着的那条 `muted` 只有读侧、没有写入口，2026-10-01 用 `/bili mute` / `/bili unmute` 补上了，见第 33 条）。
翻译（第 27、28 条）在 2026-09-30 这一轮接上了：`translate.enable` 打开、
`translate.appId` / `translate.securityKey` 填好之后，检测任务在画动态卡之前调一次百度接口，
失败只记一条日志，不影响这条动态推不推。

下面第 1、2 条是**压测覆盖**没到，不是实现没做——而且没有一条还需要 B 站登录态（这一句此前写错过，见下面「端到端自测」里 2026-09-30 的那一轮）；第 3、4 条是 2026-10-03 按你的拍板收口的两件（附件统一转码、注释表做成可枚举）；第 5 条按你的要求只解释、没动手：

1. **百度翻译的真实调用**。它挂在动态拉取之后，现在拉取通了，缺的只是一对真密钥：
   `translate.enable` 打开、`appId` / `securityKey` 填好才会出门。目前只有 `src/translate.rs`
   那 10 项单测覆盖判定口径、签名拼法与响应解析。
2. **`login` 的成功分支**。拿到 Cookie 后 `entry.update` 会把它写回 `config/bilibili/arona.yml`，
   再看框架能不能原样读回（Cookie 串里有分号、空格、等号）。这条只能你扫码，本轮压到的是失败分支。
3. **作为附件直发的图片段是什么编码——2026-10-03 按你的拍板收了：交出去的图段一律先转成 PNG**。
   先前 `cache.downloadOriginal=false`（默认）时插件先要 `…@720w.webp`、失败才回退原图，而附件发的是
   拿到的原始字节，所以实测两种都出现过：番剧/直播封面那次是 WebP（52 KB），`/bili new` 那轮是 JPEG。
   现在转码在 [`push::attachment`](src/push/mod.rs) 做，两个出口都接上了：模板里 `{draw}` / `{images}` /
   `{cover}` 那三个图片段（`push/template.rs`）与链接解析回的那张封面（`link.rs`）。
   已经是 PNG 的原样交出去（卡片与二维码自己出的就是 PNG），解不开也交原件——宁可编码不合口味，
   也好过整条推送少一张图。覆盖到的是**内联字节那一路**；唯一还交裸 URL 的是链接解析抓图失败那条退路
   （`link.rs:353`）——那份字节根本没到手，谈不上转码，而它本来也要面对图床不带 Referer 就 403 的事。
   代价一条：**体积**——PNG 无损，发出去的消息体跟着涨（实测倍数见下面那段）。
   **动图是个例外，而且是故意留的**：`Cargo.toml` 那行只给 `image` 装了 `png` / `jpeg` / `webp` 三个解码器，
   GIF 到 `load_from_memory` 那一步就 `Err`，于是照「解不开交原件」那一支把原字节发出去——动画保住了，
   代价是它是这次统一覆盖不到的那一种编码。要连它也并进 PNG 得加 `gif` feature，而那等于把动图压成第一帧，
   属于功能倒退，所以不做，只在单测里把这个行为钉住
   （[`a_gif_stays_untouched_because_there_is_no_gif_decoder`](src/push/mod.rs)：前半先断言这份 GIF 样例现在解不开，
   哪天 `gif` feature 被加上，那半句先红，逼着人显式决定要不要把动图压成第一帧）。卡片本来也画不了动图。
   盘上那份缓存仍是下载来的原件，没跟着重编码。
   体积这条有实测数，别说成"涨一个量级"：本轮链接解析工装那张 JPEG 封面是 8.0 KB → 38.7 KB（约 5 倍），
   涨几倍跟着素材走，大图只会更狠；同一轮里那三张插件自己画的卡片转码前后字节数一字不差
   （592,514 / 678,951 / 57,291），说明 PNG 早退那一支确实在原样交，没把画好的卡片重编一遍。
4. **注释表里的键名没有兜底——两头都在 2026-10-03 进了单测**。`config.rs` 那 66 条说明先前是硬编码的 `match`，
   键名写错不报错、也不让编译失败，只是生成的配置里少一行注释（上面 `lockSB` 就是这么漏进去的）。
   2026-10-02 先做了前半（[`every_key_the_template_renders_has_a_comment`](src/config.rs)：照框架 `render_node`
   那一遍走法把三块区的默认值展开成点分路径，逐个问注释表，谁没说明就带区块名列出来）。
   这一轮把三张 `match` 机械改成了可枚举的常量数组（`BILI_COMMENTS` / `IMAGE_COMMENTS` / `TEMPLATE_COMMENTS`，
   查表走 `comment_of`，键名与文案逐字照搬），后半于是也查得了：
   [`every_comment_path_is_a_real_key`](src/config.rs) 反着问「表里每一条路径是不是真有这么个键」，
   死键（改过名、删过字段）会红；`comment_tables_have_no_duplicate_keys` 查同一路径写两遍——
   第二条永远读不到，那是藏起来的抄错。三头的牙齿都现场验过：临时加一条 `check.nonexistent` 与一条重复项，
   两条新单测各红一次；再把 `admin` 的说明摘掉，前半那条红出 `["bili.admin"]`；改回全绿。
5. **一轮检测从「逐 UP」换成原插件那一次关注流——2026-10-03 按 A 做完了**。这条是 2026-10-01 拿原插件 README 的
   「## 特性」逐条回查时挖出来的，也是那张表里唯一一条本插件行为不同的：
   「无论多少订阅均可在最低 10s 内检测所有动态」——原插件那一次不是并发拉每个 UP，是**一次请求拿全部**
   （`DynamicCheckTasker.kt:38-39` 打 `x/polymer/web-dynamic/v1/feed/all`，同文件 `:41`、`:53` 再按作者 mid
   过本地订阅表），所以一轮耗时跟订阅数无关。
   落点四块：
   * `run_dynamic` 按 `bili.signed()` 分两条。有登录态走 `firehose_round`：一轮拉 1..=`check.timelinePages` 页，
     每页拼完就问 `should_fetch_more`——这一页最旧一条已经压到所有已知水位之下就不再翻（全是第一次订阅、
     一个已知水位都没有时也不翻，那一轮只对齐）。没登录态走 `per_up_round`，就是原来那个逐 UP 匿名循环，
     语义一行没改。留着它的理由是实测过的：匿名 `feed/all` 出不来内容，匿名 `feed/space` 还出。
     但这条接口也不保证每次都出——2026-10-03 那轮里三个订阅者只有 546195 的页真给了内容，另两个回的是
     `code 0` 的空页（空页不报错，所以日志里没有「动态读不出来」），数与影响见下面端到端那一节。
   * 分桶、首订对齐、跨页去重抽成纯函数 `plan_round`（不碰 `PluginContext`，也不碰 store），五条单测压在它和
     `should_fetch_more` 上。牙齿现场验过：删掉通配档那一支红两条，把 `id > watermark` 写成 `>=` 红一条，
     把翻页停止条件写死成 `false` 红一条。
   * 代价与对策：`account.autoFollow` 回到原插件那个 `true`（关注流只覆盖**已关注**的人，见第 38 条）；
     订阅表与水位仍按 uid 分档，所以「退回逐 UP」随时做得，不需要迁数据。
   * 有一条**固有代价要交代清楚**：水位推到的是「这一轮见到过的最大 id」，所以一轮里翻到 `timelinePages` 上限
     而最旧那条仍比水位新时，中间没取到的那部分就永远跳过去了（翻不够页 == 漏）。这跟原插件同构，而且它更窄：
     `DynamicCheckTasker.kt` 一轮只调一次 `getNewDynamic()`，默认 `page = 1`，一页约二十条，超出就漏，
     它靠一份 200 条 id 的环形历史（`historyDynamic`）只做去重、不做补漏。要真消掉这一档得给每档存 offset 游标，
     那是换检测模型，按规矩先给方案再动手，这轮没碰。默认 3 页 ≈ 60 条／30 秒，正常关注数撞不到。
   * 另外补了一条退路：一份坏 Cookie 会让关注流恒空，而那种状态下逐 UP 本来是出内容的，不退就是把整个检测弄停。
     `Firehose` 三态里 `Dead`（B 站判 `code -101`/`-30`，文案行头与判据共用 `api::AUTH_FAILURE` 那一个常量）
     立刻退回逐 UP，其它故障（`Failed`）连着三轮（`FALLBACK_AFTER`）才退，这样被风控那一轮不会再多打 N 次签名接口。
     两支的实测程度不一样，别混着说：**`Failed` 那一支端到端钉到了**（2026-10-03 那轮，假 Cookie 实测回 `-400`，
     第三条「读不出来」确实早于第一条 `feed/space`）；`Dead` 那一支**没有实测**——要一份**真过期**的 Cookie 才造得出
     `-101`，工装环境压根没登录态，造假又只会得到 `-400`。它现在只有两层保证：文案与判据共用同一个常量
     （改字面串两边一起变），以及 `unwrap_envelope` 那条单测钉住 `-101` 的文案行头就是这个常量。
   * **顺手更正先前写在这儿的一处错**：那句「逐 UP 是 `item.id > watermark` 的翻页水位」不对——换之前两个调用点
     都只取第一页（`space(uid, "")` 与 `timeline(1, "all")`），谁都没翻过页。真正的区别是页的**成分**：
     逐 UP 那一页全是这个人自己的动态，他被顶出首页之前不会漏；关注流那一页是所有人共用的，顶出去就是真漏。
     这才是这次要加翻页的理由，不是「原插件会翻页」。
   * 还欠的那一半：关注流拿到真数据要有登录态，工装是匿名的，所以「一轮请求数与订阅数无关」在真账号上的实测
     还欠着，跟 `#85` 那批一起等扫码。这一条里能不等登录先验的是**分支选择**与**三态退法**，已进端到端。
6. **两个「等下一句话」现在连超时那一支都端到端压到了**（2026-10-01 补的 `fake-onebot-timeout.mjs`）。
   向导那 120 秒（原插件 `ConfigService.kt` 的 `timeout(120_000)`）和 clear 那五分钟（`nextMessage(300000)`)
   都是拿 `Instant` 比的，压过期只能真空等，所以这一条一轮要 8 分钟：开向导 → 空等 130 秒 → 那句「1」
   必须**安静**（会话过期，既没被当编号、也没顺手改到配置）→ 向导能重开 → 收摊；列清单 → 空等 320 秒 →
   那句「确定」必须**安静**且条目还在（下一句 `/bili clear` 又列出同一个 g20002）→ 再发一次清单，那句
   「确定」才真删。十步、**8 条回话、可疑 0**（`bili-node-timeout.log`），跑完 `store.yml` 里 `g20002` 才消失。
   还剩一条**与原插件同语义、不是缺陷**的行为要交代：窗口内那个会话里那个人的下一句话一定被当成答复，
   所以那句「确定」不能同时是别的东西的开头。同一个人如果同时开着向导，向导注册在前会先把它吃掉当编号，
   那份清单就自己留到五分钟过期（`browse::register_confirm` 的注释记的就是这个搭配）。
   还有一处**端到端没压到、只有单测钉着**：那份「会话#发起人」的键在向导和清单两处共用，但
   `fake-onebot-config.mjs` 自始至终只有 `USER = 100` 一个人说话（这个脚本没有逐步换 `user_id` 的写法），
   所以「同群里**第二个人**发编号该安静」在向导这一路只由 `wizard.rs` 的 `a_dialog_key_carries_who_asked`
   钉住；清单那一路是真压过的（`fake-onebot-clear.mjs` 里 `STRANGER` 那两个 `quiet` 判据）。
7. **关注流那两轮的 32 条推送归因了，归出来的是一处缺陷**（2026-10-03 深夜）：`per_up_round` 那句「对齐到最新
   一条」取的是**页序**的第一条（`fresh.reverse()` 之后的 `last()`），而 `feed/space` 正是**含置顶动态**那一个
   接口——原插件 `Dynamic.kt:28` 用 `hasTop` 在 `feed/space` 与 `feed/all` 之间挑的就是这件事。置顶那条排在页首、
   id 却比近期的小，于是「第一次订阅」把水位压到那条旧动态上，下一轮整页历史全被当成新动态推出去。这一读法把
   三个数同时对齐了：第二遍唯一的动作就是那一次对齐而推送照旧 32 条、另外两个订阅者的水位整轮没动（那一轮它们
   拿到的是 `code 0` 的空页，一条没推）、落盘一次都没失败过（「状态没存下去」0 次，所以不是没存住）。
   改成按 id 取最大、推送也按 id 排（`sort_by_key`）之后复跑了：`run-firehose.sh` 的 align 模式那条硬判据
   （**只对齐之后推送该 0 条**）实测 **0 条**，改之前是 32 条，数与跑法都记在下面「端到端」那一节。
   原插件不带这个病：它的水位是**时间**而不是 id（`DynamicCheckTasker.kt:46` 那句 `it.time > lastDynamic`，
   而 `:62` 那行推进水位的代码是被注释掉的，去重全靠那份 200 条 id 的环形历史）。这边按 id 划水位是为了落盘能认账
   （`store.yml` 里那格直接是一条动态 id，`/bili list` 与排障都能读），那就必须显式取最大，不能拿页序当时间序。
   留一笔待定的小事：现在「取页里最新一条」在 `per_up_round` 与 `plan_round` 里各写了一份，语义对齐了但代码还是两处，
   要不要并成一处没定——`per_up_round` 若直接改用 `plan_round`，得先确认转发动态的 `module_author.mid` 一定等于被订的那个 uid，
   不然分桶会把那一档的作者过滤掉。

原来列在这里的第 1 条「**直播卡被真实开播数据驱动**」已经在 2026-09-30 晚间这一轮压到了：
`feed/space` 里那条直播推荐动态真推出了一张 PNG 直播卡 + 「老番茄 正在直播：最手忙脚乱的一集 /
分区：其他单机 / 房间号：21852」，直播检测那一轮也真推过「3号直播间 正在直播：哔哩哔哩音悦台」。
代价是当场挖出第 35 条那个 `null` 占位的解析缺陷——它原本是画一张空卡（房间号 0、主播名与标题都空）。

## 端到端自测

假 OneBot 端与运行目录都在本机 `C:\Users\cheng\Arona-e2e\`：运行目录 `run-bili\` 里
`config/onebot.yml` 的 self_id 是 10000，`config/arona.yml` 的管理员写 100 与 12345（那是框架的
`managers`，不是插件那区），插件 `bili.admin` 为了压通报单独写了 `[12345]`，插件 dll 放
`plugins\bilibili\`。除了 `peek-images.mjs` 每个工装都起一个 ws-forward 服务（默认 9001），一次只跑一个：

```
node fake-onebot-bili.mjs      # 命令序列：21 条本地 + 5 条联网
node fake-onebot-login.mjs     # /bili login 专测：出图 → 轮询 → 撤码
node fake-onebot-link.mjs      # 链接解析钩子：七条带 at 段的消息，含一条负例
node fake-onebot-season.mjs    # 番剧跟跑：订一部番后静默四轮，看检测有没有每轮都查
node fake-onebot-new.mjs       # 软限流探针：同一个 uid 连发 N 次 /bili new，数几次出真动态
node fake-onebot-listen.mjs    # 纯监听：不下发命令，只数定时推送与其中的图片段
node fake-onebot-cookie.mjs    # 只发 `/bili config` 的 `全局` 档，把清单全文打出来（查配置项有没有真生效）
node fake-onebot-config.mjs    # /bili config 向导：26 步状态机序列（编号、坏值、门控、退出后该安静、私聊编号前移）
node fake-onebot-clear.mjs     # /bili clear 的二次确认：21 步，含「这里该安静」的负例与两份向导/清单并存
node fake-onebot-timeout.mjs   # 两个会话的超时支路：空等 130 秒与 320 秒，看过期后那句话既不被吃也不误删（一轮 8 分钟）
node fake-onebot-mute.mjs      # /bili mute 与 unmute：写侧落库、订阅不被吃掉、权限门继承、被拒也通报、重载后状态还在
node fake-onebot-live404.mjs   # 最小诊断：直播链接之后框架还认不认后面的入站（四条老工装偶发静止时用来定位）
node fake-onebot-notify.mjs    # 操作通报与「加载中…」：发命令的人刻意用 20002（不是框架管理员）
node fake-onebot-card.mjs      # 只发一条 /bili new，把回话里的卡片图解成 png 落盘看版式
node peek-images.mjs           # 不是服务端：把 sent log 里的图片段解成「PNG/JPEG 宽x高 字节数」
```

`fake-onebot-config.mjs` 要跑两轮才盖全：`E2E_BOT_ROLE=member` 压 At全体 那格的门控拒绝（26 步），
`=admin` 压它真写库那一支并把增删查全压通（29 步，多出的三步是 `1.1` 查当前项、`1.3` 发起删除、
和那句真删掉的「直播」）。
同一个 `get_group_member_info` 对 `user_id === self_id` 返回什么角色，决定那一格走哪条路。
`bash run-config-wizard.sh` 把两轮连着跑——每轮先删 store 基线、起 node 再起宿主、
跑完 taskkill，把「问/疑」两行、落盘的 `store.yml` 和 panic 计数一起并进 `bili-config-round.log`。
`run-clear.sh` 是同一套壳跑 `fake-onebot-clear.mjs`（21 步：失效目标识别、答复只认同人同会话、确认窗口不吞命令、
一份清单只许答复一次、向导与清单并存、非管理员看不到清单；bot 在群里的身份固定是管理员）。
另外七个壳是同一条流水线的不同工装：`run-timeout.sh` 跑那条 8 分钟的超时支路，`run-cmdseq.sh` 跑整套命令序列，
`run-link.sh` 跑链接解析那七条——后两个是给「`Cmd::Config` 换向导 + 两条新 High 钩子」这类改动做回归用的；
`run-mute.sh` 跑 `/bili mute` 那十二条，跑完额外把 `store.yml` 里那几格 `muted` 的形状一起打出来；
`run-live404.sh` 是偶发静止时的最小诊断。
2026-10-03 给先前只有裸 mjs 的三套也补了壳，三套对那两样东西的处置正好相反，写在这儿是因为搞错一次就整轮白跑：
`run-login.sh` **要关 `bili.enable`**——它判据的头一条是「发过带 image 段的消息」，定时推送往里塞图会让这条判据失去牙齿；
`run-season.sh` **不关**（「静默 N 轮」等的就是 `dynamic_job` 带着 `run_seasons` 跑起来，闸掉任务等于把被测的东西关掉），
但要删 `store.yml`，上一轮留下的基线会让这轮直接进「出新集」分支；
`run-listen.sh` 两样都不做，反过来**自己铺一份带订阅、水位压在旧动态上的 `store.yml`**——
指望别的壳留订阅靠不住，`run-cmdseq.sh` 收尾那两条 `del` / `delall` 正好把 `subs` 清空。
它不下发命令，只数检测任务主动推了几条、几条带图，零推送不算失败（那段时间可能真没人发），
有牙齿的是那一行编码计数。三套壳的判定块现在都带同一行编码判据。
2026-10-03 又加了第十一套 `run-firehose.sh`，压的是「预留 5 按 A 做完」那条新主路径（关注时间线当主检测路径）。
它对那两样东西的处置跟 `run-listen.sh` 同类、跟 `run-login.sh` 相反：**绝不关 `bili.enable`**——
这一轮验的就是那几条定时任务自己选走了哪条路，闸掉任务等于把被测对象关掉。
它另外做两件 `listen` 那套不做的事：一是 `sed` 往 `account.cookie` 塞一份**假但非空**的 Cookie，
让 `bili.signed()` 为真，于是「这一轮该发 `feed/all`」这个分支能被宿主日志里的代发计数指认；
二是自己铺一份三个订阅、只给其中两个水位的 `store.yml`，那个没水位的订阅就是留给「第一次订阅只对齐」那一支的。
假 Cookie 是被 B 站拒的（实测回 `-400`，不是 `-101`），所以这一套天然把三态退法里 `Failed` 那一支也压到了：
连着三轮读不出来才临时退回逐 UP，判据是「第三条『关注时间线读不出来』的行号必须早于第一条 `feed/space`」。
它另有 `E2E_MODE` 两档，各钉一件事：默认的 `align` 钉「第一次订阅只对齐」真不真——硬判据是**那一轮推送该 0 条**
（2026-10-03 深夜之前它数到 32 条，那就是预留问题第 7 条那个缺陷，改完复跑是 0 条）；
`gated` 把三份水位全压到 i64 上界，钉的是「推送确实把水位当门」，同一句判据换一个读法。两档都不关 `bili.enable`。

读旧一轮的 round log 要留意一件事：这些壳原先用 `^(0[0-9]:…)?(问|疑):` 挑判定行，那个 `0[0-9]` 只认 00–09 点，
而 node 打的是 UTC 时间戳——所以 UTC 10 点之后跑的那几轮，round log 里只有「可疑 N 条」那一句汇总、没有逐条 `通:`。
2026-10-01 已经把七个壳都改成不带时间戳锚定的匹配，此后每一轮 round log 里逐条判定行与汇总句同时都在；
之前那些轮次要回看逐条结论，直接读同目录的 `bili-node-*.log`（那份一直是全的，只是同一个文件名一轮一份）。

这些壳都遵守同一条纪律：**宿主在跑的时候不要并发跑 cargo**。本项目早前就记过 dll/exe 被内存映射占着会让构建失败，
而 2026-10-01 还见过更邪的一次——link 工装头一遍跑到第五条起宿主完全静止（日志与 `sent` 都不再增长，假端也没收到
「框架断开」），而那正是我在同一分钟并发起了一轮 `cargo clippy/test`；挪开构建重跑同一工装，七条全部正常处理。
所以这类「连接还在、宿主却不处理入站」的偶发，第一嫌疑是并发构建，别急着报成框架缺陷。

压会话与命令面的那几套壳都在起宿主前把 `bili.enable` 改成 `false`、跑完按备份还原。
原因：**quiet 那几条判据怕检测任务真推一条**——
`bili.enable: true` 时动态每 30 秒一轮，一条「老番茄 更新了视频」落进「这一步该安静」的桶里就被记成骚扰。
2026-10-01 那一轮 A 就这么可疑 1 条，而它不是缺陷：那一段压的是命令与那句答复，本来就不该开着推送。
`bili.enable` 只闸任务（`tasks/mod.rs:81`），命令面照常，所以关掉不影响其它判据。

`peek-images.mjs` 是判「绘图开关有没有生效」的那把尺：图片段正文里 `iVBORw0KGgo` 开头是插件画的
PNG 卡片，`/9j/4AAQSkZ` 开头是动态附带的 JPEG 原图——只看有没有 `iVBOR` 就够了，比数条数可靠。

看版式其实不必跑宿主。`cargo test --lib card_preview -- --ignored --nocapture` 那条（挂在
`src/draw/card.rs` 的测试里，带 `#[ignore]`）拿假素材离线画三张——九宫格、视频附件、转发嵌套——
写到临时目录的 `arona-bili-cards/`，跟原插件 README 那三张样式预览逐格对。想让 unicode 表情也
画出来，设 `ARONA_TWEMOJI_DIR` 指向放好 `<码位>.png` 的目录，那几张就是 gather 从 twemoji 拉的同一份。
2026-10-01 这一轮就是它逼出 ❤️ 画成豆腐块那件事的：当时运行目录没有登录态，`/bili new` 回
「未找到动态」，宿主那条路反而看不到卡。

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
  配置里 `account.cookie` 留空，`subs` 加上通配档 `0`（`wants_all()` 为真）。2026-10-03 之后
  「关注时间线」已经是登录态下的检测主路径（`tasks/mod.rs:216` 那一支，不再要求有人订 `0`），
  所以**宿主日志里有没有 `feed/all` 代发**就是硬判据。
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

还有一条记在这儿：作为附件直发的那一份图，编码**曾经**是跟着图床那一次给的是哪一份走的，插件不重编码。
`fetch`（`tasks/mod.rs:1050`）在 `cache.downloadOriginal=false`（默认）时先要 `…@720w.webp`，那一次失败才回退原图；
拿到的原始字节直接塞进图片段，解码只为画图。所以同一个附件位子上实测出现过两种编码：
定时推送那次是 WebP（`base64://UklGR…`，52 KB），`/bili new` 这轮是 JPEG（`base64:///9j/4AAQSkZJRg…`）。
**2026-10-03 按你的拍板收口成 `push::attachment`：交出去的图段一律先转 PNG**，做法与两条代价见上面第 3 条。

收口这一轮手上有旧新对照，而且是同一套链接解析工装、只换了 dll：
`sent-link-olddll.log` 那份是 4 个图段——`JPEG 8143B | PNG 592514B | PNG 678951B | PNG 57291B`；
`sent-link.log` 那份是 `PNG 39581B | PNG 592514B | PNG 678951B | PNG 57291B`。
只有那一个 JPEG 头发型变了，三张卡片一个字节都没动（PNG 早退那一支），判据行从
「疑: 还有 1 处 JPEG/WebP 头发出去了，转码那一环没接上」变成「通: 抓来的图都转成了 PNG」，
整轮仍是可疑 0 条、布局 5、0 panic。
这份凭证的成因要交代清楚：那个 `olddll` 不是特意为了对照用 `--target` 编了一份旧产物，
是我第一遍拷错了目录——`cargo build --release --target x86_64-pc-windows-msvc` 的产物在
`target/x86_64-pc-windows-msvc/release/`，我拷的是 `target/release/` 里 10-01 那份旧的，于是第一轮实跑的是旧 dll。
错出来的那份判定留着当改前凭证正好合"新负例先用旧 dll 复现"的口径，但下一轮起目录按 README 开发块那行警告来。

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

2026-10-01 中午又接了两小轮，把上面那句「两个开关都开」扩到能钉住第 39 条的每个口径。
`fake-onebot-notify.mjs` 的命令表加长了两条注定失败的命令，判定从「通报 ≥ 1」抬到「通报 ≥ 3」：

- 第一轮（`sent-notify-rej.log`）：`/bili new 546195 1` 这回真出了动态——「加载中...」→ 一张 PNG 卡
  → 正文「这是一只美人鱼，你敢和她直视10秒吗？」→ 一张 JPEG 配图 → `delete_msg {"message_id":1002}`；
  通报 2 条，「结果」那一行分别是「订阅 老番茄 成功!」和「格式错误，请输入16进制颜色，如: #d3edfa」。退出码 0。
- 第二轮（`sent-notify-rej2.log`）：同一份订阅再发一次 add，于是通报 3 条覆盖三类走向——
  重复订阅（「之前订阅过这个人哦」）、参数写错（同上面那句）、身份不够
  （`/bili atall 直播 20001` → 「Bot不为管理员, 无法使用At全体」；假端给 bot 的 `role` 是 member）。
  这一轮同样出了一条占位与一条 `delete_msg`。
- 顺着 add 那支纠正一处先前写错的口径：**非管理员发 `/bili add` 是能成的**。原插件的 `checkPerm`
  管的是「能不能动这个目标」，群成员动本群的订阅本来就放开（`mod.rs:673` 那两句
  「无法操作其他分组/其他人」要指向别人的分组才触发），所以早前那轮那句「之前订阅过这个人哦」
  是重复订阅，不是被拒——第 39 条原来写「实测压的是权限不足那一支」据此改正。
- 两小轮之前先复跑了整套命令序列（`sent-regress1.log`）：**27 条回话，可疑 0 条**，宿主 0 panic，
  握手仍是「布局 5，契约 1.0.0」。比先前那轮报的 26 条多一条，多出来的就是 `/bili new` 那条
  「加载中...」占位——`showLoadingMessage` 默认开着，占位也算一条出站消息，计数口径得跟着改。
  这一轮假端只收到 `send_group_msg` 一种帧、0 条通报：发命令的人用的是 100（框架 `managers` 里的号），
  管理员自己动手不发通报，正是那三重 gate 的反例。

同一时刻给配置面补了第三证：删掉 `config/bilibili/arona.yml` 让框架按当前 HEAD 重生成，再拿两份脚本
（本机 `C:\Users\cheng\Arona-e2e\check-config-keys.mjs` 与 `check-comment-keys.mjs`）对三方——
README 那张键表的 49 个键在生成文件里全找得到，`config.rs` 注释表那 66 个键也全挂得上。
第二支脚本一跑就抓到一处真错：`ColorGenerator::lock_sb` 经 `rename_all = "camelCase"` 渲染出来是
`lockSb`，而注释表按原插件的写法（`BiliConfig.kt:181` 自己就写 `lockSB`）写的是 `lockSB`，两边对不上——
生成的配置里这一项**一行说明都没有**。第一反应是把注释表和 README 一起改成 `lockSb`（`aa5a539` 就是这么做的），
但那违背本仓既定口径：模块文档第 7 行写明键名逐字保留原插件的写法，好让老用户拿原 README 对得上，
全篇也只有这一个键带连续大写。于是退回去给那一个字段单独点 `#[serde(rename = "lockSB")]`，
`rename_all` 就不碰它了。复跑脚本是「66 键零漏」，注释回来了，键名也还是原插件那一个。
这两支脚本到 2026-10-03 已经删了——判据全搬进 `cargo test`，而且搬过去之后比脚本更宽：
`readme_config_tables_only_document_real_keys` 顶了第一证，`every_key_the_template_renders_has_a_comment`
与 `every_comment_path_is_a_real_key` 把注释表两头都查了（脚本只查「表里的键在文件里找不找得到」一个方向），
`rendered_template_parses_back_to_the_defaults` 更把框架那台渲染器直接拉进测试生成整份文件再读回来，
不再依赖本机 `run-bili/` 里恰好躺着哪一份配置。
顺手改掉同一区的另一处文案漂移：`showLoadingMessage` 的注释原先写「模板列表、搜索、最新动态、最新视频」
四条，可第 40 条讲的正是模板列表**不挂**占位——注释跟着实现缩成三条，并补了一句为什么不挂。
改完重编 dll（`ed1f0813…`，已放进运行目录的 `plugins\bilibili\`），拿新那份复跑整套命令序列：
**29 条回话，可疑 0 条**、宿主 0 panic。比上一轮多出的两条是 `/bili new` 这次真出了动态，
多的是卡片与配图两段图片帧——「未找到动态」这一轮一次没出现，与上面 `fake-onebot-new.mjs`
那一段记的软限流对得上：同一条命令在 B 站肯给数据的时候一直是通的。

第 42 条（unicode 表情补图 + 出站 Cookie 按域名收口）落完之后复跑同一套命令序列（新 dll
`c9c74663…`，日志 `bili-node-emoji.log` / `bili-host-emoji.log`）：**27 条回话，可疑 0 条**、宿主 0 panic。
收口这一改过所有出站请求共用的 `Bili::headers`，所以判据挑的是「每一路都还在发」：宿主日志 8 条代发里
`nav`、`finger/spi`、`web-interface/card`、`live_user/Master/info`、`get_status_info_by_uids`、
带 `w_rid`/`wts` 的 `feed/space` 两种签名与匿名打法都正常返回（最大的一个响应 79357 字节），
说明「只在 B 站自家域附 cookie」没把签名接口或图床那条喂料切断。这一轮 `cdnjs` 那条**没有真发出去**——
当时那个 uid 的动态没带 unicode 表情（且运行目录无登录态，`/bili new` 又一次回「未找到动态」），
所以补图那一路的凭证仍是上面记的两件：离线 `card_preview` 肉眼看到 ❤️ 画进卡，加两个 twemoji URL
`curl` 到 200。真数据那一压还是等 #85 的扫码登录。

2026-10-01 凌晨这一轮压的是两个「等下一句话」（日志
`bili-node-configA.log` / `bili-node-configB.log` / `bili-node-clear.log`）：向导 member 轮 **50 条回话、可疑 0**、
admin 轮 **57 条、可疑 0**，`/bili clear` 轮 **可疑 0**，三轮宿主各 0 panic，握手仍是「布局 5，契约 1.0.0」。
向导那两个数比第一次跑各少 9 条——少掉的 9 条是检测任务真推的动态与它的配图，就是上面「起宿主前把
`bili.enable` 关掉」那一句的来源。两轮的落库逐字段核过 `store.yml`：`typeMode: White` + `types: [视频]`、
`templates.dynamic: NoDraw`、`color: '#ff8800'`，admin 轮把 At全体 加了又删之后回到 `{}`。

`/bili clear` 那一条第一次跑是**可疑 1 条**，而且抓到的是真缺陷：`clear_subs` 只把 `subs` 那些字段清空、
不删 `targets` 里的条目，而这份清单的候选正是从 `targets.keys()` 扫出来的，于是同一个死群每轮重复上榜，
最后一步「未找到失效的群/好友」永远等不到。改成 `store::Data::remove_targets`（连条目带分组挂名一起摘）
之后跑完 `store.yml` 里 `g20002` 整条消失、只剩 `g20001`。原插件不复现这一条，因为它遍历的是
`dynamic[uid].contacts`——把 contact 从每个 uid 的名单里摘掉就没有残留。
同一轮另外钉住四件：另一个会话的人回「确定」该安静、回一句别的算放弃且放弃之后再「确定」也安静
（一份清单只许答复一次）、确认窗口不挡命令（`/bili list` 照常回表且清单还留着）、非管理员连清单都看不到。

随后给这套工装长了一段，压的是**两份「等下一句话」并存**与**清单的作用域**：向导开着时打 `/bili clear`
（命令照常走分发，同时把清单登记上），那句编号先被向导吃掉当编号用、清单不动；向导收摊之后同一句编号
才轮到清单，算放弃。另外同群里**另一个人**说「先不删」或「确定」都不该动这份清单——这一条一开始真会挂：
清单那份表只按会话存了键，路人一句「先不删」就把管理员的清单取消了（`bili-node-clear-oldkey.log`：
19 条回话**可疑 2 条**，跑完 store 里 `g20002` 还在）。改成与向导共用 `wizard::key`（「会话#发起人」）之后同一套 21 步重跑是 **19 条回话、可疑 0**
（日志 `bili-node-clear.log`），跑完 store 里 `g20002` 整条不在。
这一条也正是原插件 `fromEvent.nextMessage` 的作用域：等的是**发命令这个人**的下一句话。
改完键重编的那份 dll 又把向导两轮复跑了一遍做回归（日志 `bili-config-round.log`）：member 轮
**50 条回话、可疑 0**、admin 轮 **57 条、可疑 0**，与改键前记录的那两个数一样，落库也还是
`typeMode: White` + `types: [视频]` / `templates.dynamic: NoDraw` / `color: '#ff8800'`，宿主各 0 panic——
即「会话#发起人」这个键没有把向导自己的那 26/29 步走坏。这两轮用的是同一份产物，眼下还在运行目录里，`sha256` 前缀 `0fbb4362`。
上面几段里那些 8 位前缀就别再拿去对了——`cargo build` 每次都覆盖
`plugins\bilibili\bilibili_dynamic_plugin.dll` 这同一个路径，旧的那份早就不在盘上了；
`bili-node-*.log` 同理，同一个文件名一轮一份，落在文档里的那些数字只对当时那一次成立。
真要对，拿宿主那句「布局 5，契约 1.0.0」和 store 落库的字段——那两件是每轮重打、当场可核的。

同一份 dll 又把**另外两条老工装**复跑了一遍，因为这一轮动的是它们共同依赖的两样东西：`Cmd::Config` 换成了
向导，`register()` 里多出两条 `ListenerPriority::High` 的消息钩子。整套命令序列（21 条本地 + 5 条联网）
**27 条回话、可疑 0**（`bili-node-cmdseq.log`），其中 `/bili config 0 我的分组` 回的是「配置:」那张菜单而不是
旧清单，其余 26 条一律照原样——这就同时证明了那两条 High 钩子在「又打了一条命令」时确实放行（`wizard::is_command`
只比 `command.names` 那几个人家会打的触发词，不含子命令别名，所以「直播」「配置」这种回答不会被误当成命令）。
链接解析那条链（七条带 at 段的消息，`bili-node-link.log`）也是**七条回话、可疑 0**：解析钩子是 `Normal`
（`link.rs:88`），排在两条新钩子后面，但没有会话挂着时两条 High 都回 `Pass`，`今天天气不错` 那条负例照样安静。

`/bili mute` 那一轮是**新写的工装**（`fake-onebot-mute.mjs` 十二条，`bili-node-mute.log`）：**13 条回话、可疑 0**，
握手布局 5、panic 0。四组判据的落点分别是：不带目标那句回「静音设置完成，这个目标不再收任何推送」且不出现「为群」，
写 `20002` 那句换成「为群 20002 …」；静音之后 `/bili list` 仍是「共 1 个订阅」——写侧没吃掉订阅关系；
非管理员（10001）在另一个群里发 `/bili mute 20001`，被 `targets()` 那道现成的门拒了，而**拒绝也进了操作通报**，
两句都落在同一个窗口里；`/bili reload` 之后 `/bili list` 照旧，说明重载只清向导、不碰 store。
跑完 `store.yml` 三格形状正是设计的那样：`f100` 与 `g20001` 是 `muted: true`，`g20002` 被那句中文别名
`/bili 取消静音 20002` 翻回 `false`。
这一条**没做"改前复现"**，理由要写清：它是新增命令，上一版 dll 连这个别名都没有，拿旧产物跑只会回
「没有这个子命令」，那不构成证据。至于"静音真的不推"那一步发生在检测与推送里，本轮 `bili.enable` 是关着的，
那条由单测 `a_muted_target_never_wants_anything`（`tasks/mod.rs`）钉着。

补完这两条命令之后又把整套命令序列复跑了一遍：**可疑 0**、握手布局 5、panic 0。这一轮数到 **29 条回话**，
比上一段那句"27 条"多两条——差的不是命令，是联网那几条（尤其 `/bili new`）当轮真发出多少段：
「加载中…」占位、卡片图、正文，一条命令可以占掉三四个回话。所以**别拿回话总数当对齐口径**，
能对齐的是「可疑 0 条」和逐条 `通:`，总数只对本机那一轮成立。

同一份产物接着把 `/bili clear` 那 21 步会话工装也复跑了一遍（日志 `bili-clear-round.log`）：**19 条回话、
可疑 0**，跑完 store 里 `g20002` 整条不在、只剩 `g20001`，宿主 0 panic。这一轮不是走过场——新登记的
`mute` / `unmute` 进了 [`mutates()`](src/commands/mod.rs) 那份白名单，而白名单决定哪些命令会发操作通报，
清单那一路恰好有两条判据数的就是「谁该收到通报」（路人的 `/bili clear` 该收到、路人回「确定」不该收到）。
所以这一轮压的是**通报触发集合变大之后那两个判据没被改坏**。
链接解析那条链（七条带 at 段的消息）也在同一份产物上过了一遍，理由没那么硬：它是最后一条没在新 dll 上跑过的
老工装，留着就是"没验过"，而新增命令面确实动到了所有工装共用的那两处（`SUBS` 表与分发）。跑下来**七条回话、
可疑 0**、握手布局 5、panic 0（日志 `bili-link-round.log`）：空间/`ss`/`ep`/直播间/`t.bilibili`/`BV` 六条各有回话，
`今天天气不错` 那条负例照旧安静。其中 `ep6291908` 现在解出来是「番剧：小车队大救援：自然护卫队」——
就是站方自己换了归属的那一处（下面一段交代过：具体集名不能钉，只钉结构词）。
最后两条有壳的也补上了：`fake-onebot-live404.mjs`（最小四步，help → 直播链接 → help → 空间链接）
**5 条回话、可疑 0**；`fake-onebot-timeout.mjs` 那条要真空等八分钟的（向导那 120 秒与清单那 300 秒）**十步、
8 条回话、可疑 0**，过期之后那句「1」与那句「确定」都安静，重开一次再答才算数，跑完 store 里 `g20002`
才被真删掉。两轮都握手布局 5、宿主 0 panic。
到这儿**有壳的七套工装（八轮）全在同一份产物上过了一遍**——`cmdseq` / `config`（member 与 admin 两轮）/
`clear` / `mute` / `link` / `live404` / `timeout`。
2026-10-03 给先前只有裸 mjs 的那三套也补了壳（上面壳清单那段交代了各自怎么处置 `bili.enable` 与 `store.yml`），
于是**十套工装在「附件统一转码 + 注释表可枚举」这一份产物上全过了一遍**，这一轮跑到的五套各自拿到：
`cmdseq` **29 条回话、可疑 0**；`link` **7 条回话、可疑 0**，而它正是新旧 dll 对照的那一轮；
`season` 追番落库（store 里 `seasons: [ss38942]` 与顶层 `ss38942: '418481'`）、**静默 4 轮检测任务 0 条误推**、可疑 0；
`listen` 这一轮证据最硬——把 store 铺成「有订阅、水位压在一条旧动态上」之后，
**定时检测自己推了 32 条、其中 21 条带图，21 张全以 `iVBORw0KGgo` 开头，JPEG 与 WebP 各 0**，
跑完水位从 `1231446245684281365` 顶到 `1253703543616962617`。一次推这么多是把水位刻意压旧的结果，不是常态，
但它把「检测 → 翻页取回历史 → 逐条过滤 → 模板填充 → 节流 → 框架发图」整条链在无命令触发的情况下驱动了一遍；
`login` 四条判据全过（round log 里一行 `问:` 都没有：发过图、发过那句扫码提示、收尾撤了那两条、有回话），
13 次动作往来，那张二维码是本地 `qr_png` 画的 PNG。撤码那一步由**真过期**触发而不是本地兜底超时——
宿主日志里 poll 那一路共 59 次代发，前 58 条响应都是 119 字节，最后一条 128 字节，那一份才是 B 站给的过期。
每轮都是握手布局 5、宿主 0 panic。

2026-10-03 把关注流提成主路径（预留 5 那条按 A 做完）之后，新壳 `run-firehose.sh` 连跑两遍、`run-cmdseq.sh` 复跑一遍，
产物都是这一份 A dll。关注流那一轮的判定（`bili-firehose-round.log`，宿主日志 `run-bili/bili-host-firehose.log`）：

* **分支选择钉到了**：一个约 230 秒的窗口里 `feed/all` 代发 **7 次**，宿主那句「关注时间线读不出来」**7 条**，
  两个数一致就说明每一轮都先去打关注流；`check.interval` 是 30 秒，7 次对得上那 7 轮。
* **三态退法里计数那一支（`Failed`）钉到了**：第三条「读不出来」在宿主日志第 40 行，第一条 `feed/space` 在第 43 行，
  判定行是 `认: 连着三轮读不出来之后才退回逐 UP`。退回之后 `feed/space` 共 **15 次 = 5 轮 × 3 个订阅**，
  三个 uid 各 5 次——退的确实是整套逐 UP，不是只补一个订阅。这一套用的是**假 Cookie**，实测被 B 站拒成
  `code -400: 请求错误`（不是 `-101`），所以走的是「连着三轮才退」那一支；`Dead`（`-101`/`-30` 立刻退）这一支
  工装造不出来，见上面预留 5 里那段交代。
* **第一次订阅只对齐这一条差点白跑**：头一遍把「没铺水位」的那个订阅者写成 168887495，那一轮这一句是 **0 次**。
  不是逻辑坏——那一带 `feed/space` 在这套工装里回的是 **`code 0` 的空页**（15 次请求、一句「订阅者 X 的动态读不出来」
  都没有），空页连 `fresh.is_empty()` 这一关都过不去，走不到对齐那一支。改成把无水位铺在 **546195**（实测匿名页
  真出内容的那个）之后，第二遍拿到一行 `bilibili：546195 是第一次订阅，历史动态不推，水位已对齐最新一条`。
  **而这两遍各数到 32 条主动推送，正是这一句没兑现**——预留问题第 7 条记着成因（`feed/space` 含置顶，
  水位按页序取了那条旧动态）与修法，改完的复跑数在下面。
* **跳过轮 1 次**（`上一轮动态检测还没跑完，这一轮跳过`）：那一轮末尾在排 32 条推送的队，一轮拖过了 30 秒，
  这正是 `Busy` 那把锁该跳的情形，不是卡死。缺陷修掉之后同一判据数到 **0 次**，两边互为印证。
* 编码那三行照旧：那 32 条里的 **21 张全以 `iVBORw0KGgo` 开头，JPEG 与 WebP 各 0**；握手布局 5、宿主 **0 panic**。
* **新键的回读实测是「0 次」**：跑着的那份配置文件里始终没有 `timelinePages`。这不是缺陷而是口径——框架只在
  配置文件不存在时渲染模板，已有的文件不动；缺键由 `#[serde(default)]` 读回默认值 3，所以老配置文件一个字不改也能跑
  （这一轮就是这么跑的），要让新键显出来得删掉文件让它重生成。顺带一条**写壳的教训**：第一版把这条判据写在
  「按备份还原配置」那一步之后，还原用的是跑前那份，所以结构上永远是 0；改成还原之前取数，才拿到上面那个真的 0。
  「读回默认 3」这半句不是推的：同一份 A dll 上复跑的 `config` 那两轮里，向导「全局」那一页实测打出
  `命令名 /bili｜检测间隔 动态 30 秒 / 直播 15 秒｜低频 3-8x2｜关注流翻页 3 页`，而运行目录那份配置里压根没这个键。

`cmdseq` 在同一份 A dll 上复跑：**29 条回话、可疑 0**、握手布局 5、0 panic（`bili-cmdseq-round.log`）。
其中 `/bili new 546195` 照旧回出「这是一只美人鱼，你敢和她直视10秒吗？」——那一套的 `account.cookie` 是空的，
所以它压的正是「没登录态时逐 UP 那一路语义一行没改」。`config` 那两套（member 与 admin 两轮）也在同一份 A dll 上
复跑过：**50 条与 57 条回话、可疑各 0**、两轮都是握手布局 5、宿主 0 panic，向导的状态机与 `At全体` 那格的两条路
没被这次改动碰坏。

上面那 32 条推送**归因归到底了，而且归出的是一处缺陷**（2026-10-03 深夜，细节在预留问题第 7 条）：
钉住它的是三样东西——宿主日志里「546195 是第一次订阅」在第 71 行、第一条推送在第 98 行，对齐在先、重推在后；
`per_up_round` 取「最新一条」用的是页序而不是 id；而原插件 `Dynamic.kt:24` 那句 `@param hasTop 是否包含置顶动态`
配上 `:28` 的 `if (hasTop) SPACE_DYNAMIC else NEW_DYNAMIC`，写明了 `feed/space` 就是**含置顶动态**那一个接口，
置顶那条 id 比近期的小、却排在页首。改法就两行：水位按 id 取最大、推送按 id 排。
改完重编那份 dll 复跑同一套壳、同一份 align 夹具（546195 不铺水位），
窗口约 230 秒，判定行是 `认: 546195 没铺水位，对齐到最新一条之后一轮都没重推历史（0 条）`：
**推送 32 条 → 0 条**，`feed/all` 代发 **8** 次对 **8** 条「读不出来」，`feed/space` **18** 次 = 6 轮 × 3 订阅，
对齐那一行 **1** 次，跳过轮 **0** 次，握手布局 5、宿主 0 panic。
`store.yml` 里 `546195` 的水位照旧是 `1253703543616962617`——同一个数在两次跑法里含义不同：
修复前它是被那 32 条推送顶上去的，修复后它是「只对齐」那一步直接写的。这一步分得开，才是这条判据有牙齿的地方。
紧接着用同一份产物跑了一遍 `E2E_MODE=gated`（三份水位全压到 i64 上界）：约 170 秒的窗口里 `feed/all` **6** 次对
**6** 条「读不出来」、`feed/space` **12** 次 = 4 轮 × 3 订阅，退回那一支的判据行照旧是 `认:`，
而推送 **0 条**、对齐那一行 **0** 次（三份水位都有值，走不到对齐），跑完三份水位原封不动停在 `9223372036854775807`——
推送确实把水位当门。握手布局 5、宿主 0 panic，两档都是。

同一份 A dll（**修复之前那一份**——八套老工装的轮志都是 22:4x–23:0x 之间落的，两行改动是之后才编进去的）
上，先前那八套老工装也复跑了一遍做回归（`bili-{clear,mute,link,live404,season,listen,login,timeout}-round.log`）：
`clear` **19 条回话、可疑 0**，二次确认那两条（「确定」/「先不删」）管理员与路人各走一支，`g20002` 整条从 store 里消失；
`mute` **13 条回话、可疑 0**，`muted` 三格各归其位（g20001 true、给 g20002 取消回 false、f100 true），中间夹一次
`/bili reload` 之后清单照旧；`link` **7 条回话、可疑 0**，抓来的图 **4 张全以 `iVBORw0KGgo` 开头，
JPEG 与 WebP 各 0**；`live404` 那条最小诊断 **5 步全通、可疑 0**，直播链接之后入站照旧被处理；
`season` 追番落库（store 里 `seasons: [ss38942]` 与顶层 `ss38942: '418481'`）、**静默 4 轮检测任务 0 条误推**、可疑 0；
`listen` **32 条主动推送、21 张全 `iVBORw0KGgo`**，水位从 `1231446245684281365` 顶到 `1253703543616962617`；
`login` 四条判据全过、13 次动作往来、那张二维码 1 张 PNG；`timeout` **8 条回话、可疑 0**，两条「等下一句话」
都真在超时之后安静下来（向导与 `/bili clear` 各一支）。八轮都是握手布局 5、宿主 0 panic。

`listen` 这一轮顺手把那处缺陷的**对照组**送上了：它铺的水位是旧的、订阅表里只有 546195 一个人，推出来的 32 条
与关注流那两遍**逐条相同**（连图片段头部那串尺寸都对得上）。所以那 32 条只能是 546195 那一页的历史——
关注流那一遍里它明明只走了「第一次订阅只对齐」，却还是被整页重推，正是水位压到了置顶那条旧动态上。

这一轮顺手把 link 工装里两处**过期夹具**改了，都不是缺陷：`pgc/view/web/season?ep_id=6291908` 现在站方
自己回的是另一部番（直接 curl 那个接口能复现，演员表是美国那部），所以「最新：第9话 发烧不出门」这种具体
集名不能钉，改成钉结构词「最新：第」；`t.bilibili.com/…` 那条指的是 9 月 30 日那条直播推荐动态，直播早下了，
而且连打七个链接时第五条常撞 `code -352`（第 36 条那个软限流），插件老实回「解析失败 被 B 站风控拦了」——
这也是正当回话，所以那条改成只要求「有回话」，牙齿留给原先那两个 forbid（空主播名、`房间号：0`）。
还要交代一次**没复现的观察**：link 头一遍跑的时候，宿主从直播那条之后完全静止（日志和 `sent` 文件都不再涨，
假端也没收到断开），三条消息石沉大海，判成可疑 3。我随后单独起了一条最小工装（`fake-onebot-live404.mjs`：
`/bili help` → 直播链接 → `/bili help` → 空间链接）**五步全通、可疑 0**，把同一套 link 工装挪开重跑也是七步全处理。
差别只在于头一遍我在宿主跑着的时候**并发起了一次 cargo**——那是本项目纪律明令禁止的组合，所以那次静止的第一嫌疑
是它，不记成框架缺陷，也不据它改框架。

上面讲 `lockSB` 那一段的两支脚本一直只在我本机，`cargo test` 管不到它们。这一轮把其中一支的判据搬成了
单测 [`every_key_the_template_renders_has_a_comment`](src/config.rs)：`config.rs` 的注释表是硬编码 `match`，
键名写错既不报错也不警告，只是生成出来的配置里悄悄少一行说明。单测不碰那张表的结构，只照框架
`render_node` 那一遍走法（非空映射先问自己再逐个子键下沉，数组和标量只问到这一层）把三块区的默认值展开成
点分路径，逐个去问注释表，谁没说明就带区块名列出来。四张自由键名的表（`cache.expires` 与模板那三张）
只问到表本身那一句，不下钻——那些键是用户自己起的名字。
它的牙齿是现场验的：把 `"colorGenerator.lockSB"` 临时改成 `"colorGenerator.lockSBx"` 重跑，报的正是
`["bili-image.colorGenerator.lockSB"]`，改回来即绿；三门跟着走完（fmt 0、clippy 0、全绿，测试总数见「开发」那节）。
**这条改动只在测试里**，产品码一行没动，所以上面那些端到端结论说的还是运行目录里那份 dll，
不必重编也不必重跑。反向那半仍然空着：表里多出一条配置里已经没有的死键，得先把表做成可枚举的才查得了。
同一天紧接着把本机那两支脚本都搬进了 `cargo test`——命令表与 `SUBS` 的对照见「功能面差集收口」命令面那一条，
配置键表与渲染回读见配置面那一条。

## 功能面差集收口

判"改完了"用的是三面差集，每一面都比到具体条目，不是比总数：

- **命令面**：`DynamicCommand.kt` 有 33 个 `@SubCommand`，本插件 [`SUBS`](src/commands/mod.rs) 35 行——
  那 33 行规范名逐条对齐（把驼峰统一成小写再比，diff 为空），多出来的两行是本插件自己的
  `mute` / `unmute`（第 33 条：`muted` 这个字段读侧早就有、写侧一直没有，2026-10-01 补上入口）。
  上面那张命令表也照样和 `SUBS` 比过一次，35 行对 35 行、两个方向都是空。**别名那一列也逐条比过**，
  而且 2026-10-02 起这一整件事是单测了：[`readme_command_table_matches_the_subcommand_rows`](src/commands/mod.rs)
  把 README 那一节的表读进来，按名字对齐 `SUBS`，再比别名集合与用法串，两个方向都查（原先只靠本机
  `check-alias-keys.mjs`，`cargo test` 管不到它；那支脚本 2026-10-03 已删，判据就在这条单测里）。牙齿同样是现场验的：临时把 `laa` 改成 `laaz`、
  再把表里 `Mute` 那行的名字改成 `Mut`，一次报出三条——「README 有 Mut，SUBS 里没有这一行」
  「ListAtAll 的别名对不上：README {…laa…} / SUBS {…laaz…}」「SUBS 有 Mute（别名 […]），README 表里没这一行」。
  说明那一列故意不钉：bot 里回的那条要短，文档里那条要讲清，`Config` 那两行本来就长得不一样。
  别名写错不会编译失败，用户照文档敲却命不中，所以只能这么钉。别名表除了照抄的那批
  还补了中文写法，比对走 `to_lowercase`，所以原插件的 `delAll` / `listAll` / `filterType`
  这些驼峰写法在这边同样命中。
  不依赖 chat-command：命令名、别名、参数位次都在这份表里自己判。
  **但名字对上了不等于语义对上了**：`/bili config` 先前这边只做到"回显一份清单"，原插件那一条却是
  交互式配置向导（`ConfigService.kt` 的 `whileSelectMessages`）。2026-10-01 按第 12 条补成状态机，
  四格菜单与 `x.1`~`x.5` 子编号照原样，旧那份清单挪到 `全局` 那一档后面。这条是差集比到条目级之后
  仍然漏掉过一次的那一类——所以其余各条的行为取舍也都逐条写进上面那份「已知差别」，不再靠总数对齐。
- **配置面**：`BiliConfig.kt` 的 `EnableConfig` 8 个开关逐个有下落——`drawEnable`（`bili-image` 那区）、
  `liveCloseNotifyEnable`、`translateEnable`、`notifyEnable`、`showLoadingMessage` 是真开关，
  `lowSpeedEnable` / `cacheClearEnable` / `proxyEnable` 在这边改成"留空即不开"（`check.lowSpeed` 空串、
  `cache.expires` 全 0、`proxy` 空表），语义等价而少一个键。`CheckConfig` / `PushConfig` /
  `TemplateConfig` / `CacheConfig` / `LinkResolveConfig` 的键逐条过，`TemplateConfig` 那几档模板名
  一档不改名（第 37 条）。**默认值也逐键比过**（2026-10-01）：拿 `BiliConfig.kt` 每个 data class 的
  默认值对着这边 `Default` 实现看，凡不同的都能指回第 38 条那份清单——没有"移植时悄悄改错一个数"的那种。
  2026-10-02 这一面也进了 `cargo test`，两条：
  [`readme_config_tables_only_document_real_keys`](src/config.rs) 把 README 那几张键表读进来，
  每个键都必须在框架真会渲染出来的那些路径里（临时把 `translate.cutLine` 写成 `translate.cutline` 就报
  `["translate.cutline"]`，大小写漂移也抓）；
  [`rendered_template_parses_back_to_the_defaults`](src/config.rs) 是直接拿框架那台渲染器
  （`typed_section::<T>(key).render(...)`）把三块区各生成一份，再要求生成的文本**读回来等于整套默认值**——
  这条钉的是「框架自己生成的配置文件自己读不回来」那一类（带换行的值早先被渲染成块标量，框架侧已修）。
  这两条都只查一个方向：文档写了却不存在会红，代码里有而文档漏了仍然查不了——那一头归注释表那三条管，
  而它从 2026-10-03 起是两头咬合的：配置里有键没说明会红（`every_key_the_template_renders_has_a_comment`），
  表里多出一条已经不存在的键也会红（`every_comment_path_is_a_real_key`），
  同一路径写两遍（第二条永远读不到）由 `comment_tables_have_no_duplicate_keys` 管。
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
cargo test --lib               # 186 项（183 通过 + 3 项标了 #[ignore]：两条联网、一条离线绘图）
cargo build --release          # 加了 --target 三元组时产物在 target/<三元组>/release/，不是 target/release/——
                               # 那边留的是上一轮的旧 dll，拷错目录会拿旧产物当新结果用（2026-10-03 真踩过）
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
