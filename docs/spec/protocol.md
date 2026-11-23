# sigil-stash-kv TCP プロトコル仕様（初版: Version 1）

[ADR 0059](https://github.com/goodnasubi/sigil/blob/947815bfa367354dec99bd6308b8931949dec49c/docs/decisions/0059-kv-store.md) 決定 2・3 に基づく、`sigil-stash-kv` の長さ付きバイナリプロトコル仕様。

## 1. フレーム構造

すべてのフレームは 4 バイトのフレーム長で始まり、プロトコル版、種別（操作または結果）、種別ごとの本文が続く。

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Length (32-bit BE)                   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  Version (8)  |   Type (8)    |         Payload...            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

- **Length (32-bit unsigned big-endian)**:
  - `Version` からフレーム末尾までのバイト数（`2 + Payload.len()`）。
  - 有効範囲: **2 〜 4096** バイト（全体の電文長は 6 〜 4100 バイト）。
  - 確保前に検査し、範囲外（< 2 または > 4096）の場合は即座に接続を切断する。
  - キー長（<= 512）や値長（<= 3072）が単体で上限内であっても、フレーム全体の長さが 4096 バイトを超える電文は不正長として即座に切断される。
- **Version (8-bit unsigned)**:
  - プロトコル版。初版は `1` (0x01)。
  - `1` 以外は未対応版として接続を切断する。
- **Type (8-bit unsigned)**:
  - 要求時は操作種別（Op）、応答時は結果種別（Result）。

---

## 2. 要求フレーム（クライアント → サーバー）

1 接続につき一度に 1 要求を処理し、要求と応答は 1 対 1 とする。

| 操作 | Type | 本文構造 | 説明 |
|---|---|---|---|
| `PING` | `0x01` (1) | なし（0 バイト） | 接続確認。応答は `Success`。 |
| `GET` | `0x02` (2) | `key_len (u16 BE)` + `key (Bytes)` | キーの値を取得。応答は `Value`、`NotFound`、または `Error`。 |
| `PUT` | `0x03` (3) | `key_len (u16 BE)` + `key (Bytes)` + `val_len (u16 BE)` + `val (Bytes)` + `ttl_tag (u8)` (+ `ttl_sec (i64 BE)`) | 値と期限を原子的に設定。応答は `Success` または `Error`。 |
| `DELETE` | `0x04` (4) | `key_len (u16 BE)` + `key (Bytes)` | キーを失効・削除。応答は `Success` または `Error`。 |

### TTL の符号化
- `ttl_tag = 0x00`: 期限なし（`NoExpiry`）。本文末尾に追加フィールドなし。
- `ttl_tag = 0x01`: 秒数指定（`Seconds`）。続いて 8 バイトの符号付きビッグエンディアン整数 `ttl_sec` (i64 BE) を配置。
- それ以外の `ttl_tag` は引数誤り（`Error` コード 2: `InvalidPayload`）とする。
- `ttl_sec <= 0` は不正な TTL（`Error` コード 9: `InvalidTtl`）とする。
- `ttl_sec` による期限計算があふれる場合は `Error` コード 10 (`DeadlineOverflow`) とする。

---

## 3. 応答フレーム（サーバー → クライアント）

| 結果 | Type | 本文構造 | 説明 |
|---|---|---|---|
| `Success` | `0x01` (1) | なし（0 バイト） | 操作成功（PING, PUT, DELETE）。 |
| `Value` | `0x02` (2) | `val_len (u16 BE)` + `val (Bytes)` | GET 成功、値あり。 |
| `NotFound` | `0x03` (3) | なし（0 バイト） | GET でキーが存在しない、または期限切れ。 |
| `Error` | `0x04` (4) | `code (u8)` + `msg_len (u16 BE)` + `msg (Bytes)` | 型付きエラー。 |

### エラーコード一覧
| コード | 識別子 | 説明 |
|---|---|---|
| 1 | `UnknownOp` | 未知の操作種別（Type が 1〜4 以外） |
| 2 | `InvalidPayload` | 引数の誤り（長さ不整合、不正な TTL タグ等） |
| 3 | `KeyLength` | キー長上限外（1〜512 バイト外） |
| 4 | `ValueLength` | 値長上限外（0〜3072 バイト外） |
| 5 | `EncodedKeyLength` | 核のキー符号化後上限超過（> 1024 バイト） |
| 6 | `EncodedRowLength` | 核の行符号化後上限超過（> 4096 バイト） |
| 7 | `Capacity` | ストア容量上限到達（新規キー追加不可） |
| 8 | `TtlRequired` | TTL 必須モードで TTL なしの書き込み |
| 9 | `InvalidTtl` | 不正な TTL（<= 0） |
| 10 | `DeadlineOverflow` | 期限計算オーバーフロー |
| 11 | `ClockUnavailable` | 時計取得失敗 |
| 12 | `ClockRegressed` | 時計逆行検知 |
| 13 | `Stopped` | 永続ストア停止中（CommitUnknown 等の障害復旧前） |
| 14 | `Internal` | 内部エラー（DB エラー等） |
| 15 | `InvalidCapacity` | 不正な容量指定（<= 0） |

---

## 4. 上限値とタイムアウト

| 項目 | 規定値 / 上限 | 超過・違反時の挙動 |
|---|---|---|
| フレーム長 (`Length`) | 2 〜 4096 バイト | メモリ確保前に検査し接続切断 |
| プロトコル版 (`Version`) | `1` のみ | 接続切断 |
| 同時接続数 | 64 | 超過時は即時切断 |
| 読み出しタイムアウト | 5000 ms | 接続切断 |
| 書き込みタイムアウト | 5000 ms | 接続切断 |
| キー長 | 1 〜 512 バイト | `Error(KeyLength)` 応答 |
| 値長 | 0 〜 3072 バイト | `Error(ValueLength)` 応答 |

---

## 5. 切断方針とセキュリティ

1. **切断条件**:
   - `Length < 2` または `Length > 4096`
   - `Version != 1`
   - 読み取りタイムアウト（5 秒以内に電文が完了しない）
   - クライアント切断（EOF）または通信エラー
   - 同時接続数上限（64）の超過
2. **エラー応答で継続する条件**:
   - 未知の操作種別（`UnknownOp`）
   - フレーム内の引数不正（`InvalidPayload`）
   - ストア操作エラー（`KeyLength`, `Capacity`, `TtlRequired`, `InvalidTtl`, `DeadlineOverflow` 等）
3. **セキュリティ**:
   - 待ち受けは既定でループバック（`127.0.0.1`）。
   - キー・値・セッション ID はログや診断メッセージに出力しない。
