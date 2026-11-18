# 启动器

三个进程组成一次联机：`crab-feast-server`（权威服务器）+ 每个玩家一个 `crab_feast_pc`。

## 配置文件 `launcher.json`

放在仓库根目录（也可用 `--config <路径>` 或环境变量 `CRAB_LAUNCHER_CONFIG` 指定）。
查找顺序：`--config` → `CRAB_LAUNCHER_CONFIG` → 当前目录 → 可执行文件同目录 → 仓库根目录。

```json
{
  "server": { "addr": "127.0.0.1:5099" },
  "default_player": "player1",
  "players": [
    { "name": "player1", "client_id": 11, "window": [0, 60] },
    { "name": "player2", "client_id": 12, "window": [1280, 60] }
  ]
}
```

| 字段 | 说明 |
| --- | --- |
| `server.addr` | 服务器监听地址，也是客户端默认要连的地址 |
| `default_player` | 客户端不传 `--profile` 时用哪个玩家位 |
| `players[].name` | `--profile` 的名字 |
| `players[].client_id` | netcode client id，**每个客户端必须唯一**，撞了会互相顶掉 |
| `players[].window` | 窗口左上角 `[x, y]`，同机多开时错开窗口用 |
| `players[].server` | 该玩家单独指定的服务器地址（覆盖 `server.addr`） |
| `players[].exit_after` | 跑够多少秒自动退出，冒烟测试用 |

## 用法

```bash
# 一键：按配置起服务器 + 所有玩家窗口（父进程起完就退出）
./target/debug/crab_feast_pc --launch-all

# 或者分开起
./target/debug/crab-feast-server                 # 读 server.addr
./target/debug/crab_feast_pc --profile player1   # 读 players[0]
./target/debug/crab_feast_pc --profile player2   # 读 players[1]
```

命令行参数优先级高于配置文件：`--server` / `--client-id` / `--window` 都能临时覆盖。

## 调试参数

| 参数 | 说明 |
| --- | --- |
| `--screenshot <png>` | 启动 8 秒后把窗口画面存成 png（比系统截图可靠，Bevy 自己抓帧） |
| `--exit-after <秒>` | 跑够指定秒数自动退出 |
| `--auto-walk <秒>` | 自动向前走指定秒数，冒烟测试用 |
| `--log-state` | 服务器每秒打印所有玩家位置 |
