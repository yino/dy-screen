# 浏览器会话直播间解析规格

## Purpose

定义公开抖音直播页在原生 HTTP 受限时使用持久化系统 WebView 解析、人工访问验证、会话恢复、最小数据读取和窗口安全隔离的行为。

## Requirements

### Requirement: 使用持久化系统 WebView 解析公开直播页
系统 SHALL 使用一个全局持久化系统 WebView 作为公开抖音页面的浏览器解析通道，SHALL 让多个主播串行共享该会话，并 MUST NOT 为每个主播创建独立浏览器数据副本。

#### Scenario: 原生通道访问受限
- **WHEN** 原生 HTTP resolver 将公开直播页分类为 `access_restricted`
- **THEN** 系统通过共享 WebView 导航到规范化直播间 URL，并在同一浏览器上下文内继续解析

#### Scenario: 多个主播同时请求浏览器解析
- **WHEN** 多个 worker 在浏览器会话忙碌时提交房间解析
- **THEN** 系统按可取消队列串行处理请求，且 FFmpeg 录制任务继续独立并发

#### Scenario: 一个房间正在等待人工验证
- **WHEN** 共享 WebView 已为任一房间进入 `verification_required`
- **THEN** 系统暂停全部后续浏览器导航并保留当前验证页，但其他主播仍可执行受节流保护的原生 HTTP 检查；仅当其原生检查也受限时才进入可恢复等待，已经运行的 FFmpeg 继续录制

#### Scenario: 应用重新启动
- **WHEN** 应用重启且用户没有清除浏览器会话
- **THEN** 系统复用平台 WebView 的持久会话数据，但重新探测访问状态而不假定会话仍有效

### Requirement: 只读取受支持的最小页面快照
系统 SHALL 只从受信抖音顶层页面读取规范化 URL、标题、加载状态、安全 marker 和包含受支持初始化数据的有限脚本集合，并 MUST NOT 向远程页面开放通用 Tauri command 或任意本地文件能力。

#### Scenario: 页面包含受支持房间数据
- **WHEN** WebView 主文档包含可由现有严格 parser 验证的 `self.__pace_f.push` 房间脚本
- **THEN** 系统在内存中解析并返回与原生通道相同的直播或离线模型

#### Scenario: 导航后仍读取到旧房间文档
- **WHEN** 快照请求代次已过期、规范化 URL 不等于目标直播间、页面尚未完整加载或房间对象的受支持父级 `web_rid` 不匹配目标
- **THEN** 系统只记录 `pending` 并继续有界探测，不返回旧页面的直播流且不启动 FFmpeg

#### Scenario: 页面返回未知结构
- **WHEN** 页面没有访问限制 marker 且在有界探测期内没有受支持房间结构
- **THEN** 系统返回 `layout_changed`，不读取完整页面正文或宽泛猜测房间对象

#### Scenario: 页面脚本包含敏感流参数
- **WHEN** 最小脚本快照包含签名直播流 URL
- **THEN** 系统只在本次解析内存中使用该数据，并禁止将快照、原始 URL 或查询参数写入日志、事件、错误或数据库

### Requirement: 限制验证窗口的导航和本地权限
系统 MUST 只允许验证窗口加载 HTTPS 抖音官方页面、精确的空白子框架以及字节官方验证码 iframe 路径，MUST 拒绝其他 scheme、非白名单 host、任意新窗口和下载，并 SHALL 让该窗口不具备主窗口 capability；房间解析目标仍 MUST 严格限定为 `live.douyin.com`。

#### Scenario: 页面导航到受信抖音地址
- **WHEN** 当前标准页面流程导航到 `douyin.com` 或其子域的 HTTPS 地址
- **THEN** 系统允许导航并继续使用同一持久浏览器会话

#### Scenario: 页面请求外部顶层导航
- **WHEN** 页面尝试导航到非抖音 host、非 HTTPS scheme 或打开新窗口
- **THEN** 系统拒绝该请求，且不把目标交给本地命令或文件处理器

#### Scenario: 验证页加载官方验证码 iframe
- **WHEN** 抖音访问验证中间页创建 `about:blank` 子框架并导航到 `https://rmc.bytedance.com/verifycenter/captcha/` 下的官方组件
- **THEN** 系统允许该受限子框架完成标准加载和人工交互，但不把验证码域作为房间解析目标，也不向其开放 Tauri capability

### Requirement: 以人工处理作为访问验证兜底
系统 SHALL 优先让系统 WebView 自动执行公开页面的标准脚本；仅在有界探测后页面仍要求用户交互时 SHALL 进入 `verification_required`，并 MUST NOT 自动识别、模拟或委托第三方处理访问验证。

#### Scenario: 浏览器自动进入受支持直播页
- **WHEN** WebView 无需用户操作即可从中间状态进入受支持房间页面
- **THEN** 系统自动完成解析并隐藏验证窗口

#### Scenario: 页面持续要求用户交互
- **WHEN** 有界探测结束后页面仍带有访问限制 marker
- **THEN** 系统保持同一页面、发布需要访问验证状态并等待用户主动打开窗口

#### Scenario: 用户完成访问验证
- **WHEN** 已显示的验证窗口当前页面重新出现与目标 `web_rid` 匹配的受支持房间结构
- **THEN** 系统不重新导航即可自动确认会话恢复、隐藏验证窗口并唤醒全部等待 worker

#### Scenario: 用户关闭验证窗口
- **WHEN** 用户尚未完成处理便关闭或隐藏验证窗口
- **THEN** 系统保留浏览器会话和主播配置、停止密集重试，并保持可恢复的等待状态

### Requirement: 管理和清除浏览器会话
系统 SHALL 提供访问状态查询、重新检查、显示验证窗口和清除会话操作；清除操作 SHALL 取消浏览器解析请求并删除平台 WebView browsing data，但 MUST NOT 删除主播、录像或业务设置。

#### Scenario: 用户检查访问状态
- **WHEN** 用户触发访问状态检查
- **THEN** 系统先对当前待处理直播间执行一次受节流保护的原生检查；原生成功时清除旧验证目标、隐藏窗口并唤醒等待 worker，原生仍受限时才分配新请求代次并执行受控浏览器重新导航与有界探测

#### Scenario: 用户清除会话
- **WHEN** 用户确认清除抖音浏览器会话
- **THEN** 系统取消当前和排队解析、清理 browsing data、进入 `session_expired`，并保留全部业务数据

#### Scenario: 清除时已有录制运行
- **WHEN** 用户清除会话且 FFmpeg 正在使用已经获得的直播流
- **THEN** 当前 FFmpeg 继续运行，只有后续直播页检查或流地址刷新需要建立新浏览器会话
