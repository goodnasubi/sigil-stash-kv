# sigil-stash-kv

[ADR 0059](https://github.com/goodnasubi/sigil/blob/947815bfa367354dec99bd6308b8931949dec49c/docs/decisions/0059-kv-store.md) 段 1 のメモリ専用 KV。公開操作は `stash.memory` の `get`、`put`、`delete`、`ping`。値はバイト列で、GET は `Result<Option<Bytes>, KvError>`、更新と PING は `Result<Ack, KvError>` を返す。

`new_memory(capacity, require_ttl)` で最大エントリー数と TTL 必須の有無を指定する。キャッシュ・セッション用途では `require_ttl` を有効にする。期限切れのキーへの GET はその枠を解放する。満杯時は既存キーの置換と空き枠の再利用を許し、新規キーを収容できなければ `KvError.Capacity` を返す。

キーは 1〜512 byte、値は 0〜3072 byte。固定参照版の [sigil-db の型符号化](https://github.com/goodnasubi/sigil-db/blob/9483e36f08c35bfc62b3beeb008a531887506037/db/types.sigil) に従い、キーは `5 + key_len`、行は `2 + (5 + key_len) + (5 + value_len) + 9` で符号化後サイズを検査する。行サイズはキー、値、期限を 1 列ずつ持つ前提で、期限はタグ付き Int 一つ分を計上する。公開上限での符号化後サイズはそれぞれ 517、3605 byte で、核の 1024、4096 byte 上限に収まる。

## 時計の契約

期限付き PUT と期限付きエントリーの GET は、渡されたコールバックから単調時刻を取得する。同じストアへの操作には同じ起点の非負ナノ秒を返す時計を使う。TTL の単位は正の整数秒。時刻取得の失敗、逆行、期限計算のオーバーフローは型付きエラーになる。時計取得から期限判定、値と期限の置換、失効時の削除まではストアの排他的な可変借用内で行う。

Sigil の ADR 参照版 `0c1c441e60933fadbe05e1c5c63b11bcf4ad4409` と ADR マージ版 `947815bfa367354dec99bd6308b8931949dec49c` を確認した。公開の `clock.unix_seconds()` は永続モード向けの UTC Unix 秒を返す。内部の `std.os.io.io_now()` はスケジューラ用で、利用者コードから公開時計として利用できず、内部の時計読み取りも取得失敗を値として返さない。この段の実装は時計を注入して検証する。実運用の時計アダプターには、取得失敗を型で返し、同一起点の非負ナノ秒を公開する Sigil 側の契約が必要になる。評価器での時刻・失敗注入とネイティブ側での単調時計取得を、その契約で扱えることが必要になる。

## 永続アダプター（段 2）

`stash.persistent` は [sigil-db 核](https://github.com/goodnasubi/sigil-db) の型付き関係 API とトランザクション API だけに依存する薄いアダプターで、段 1 と同じ操作契約（`get`、`put`（期限あり／なし）、`delete`、`ping`）を永続化で提供する。SQL・PostgreSQL 通信・DB サーバーの起動、内部ページ形式への直接アクセスは使わない。

- 表 `kv(key Bytes 主キー, value Bytes, deadline Int)` の 1 行に値と絶対期限を置く。`deadline` は UTC Unix 秒で、`-1` は期限なし。1 操作が核の直列トランザクション 1 件なので、値と期限は必ず一緒に置き換わる。置換時は前の期限も置き換わる。
- `open_persistent(dir, db_id, require_ttl)` はディレクトリを開き、核の復旧（WAL 再生）を終えてから返す。表は初回だけ作る。`db_id` は新しいディレクトリを作るときだけ使う 16 byte の識別子。`close_persistent` で排他ロックを手放す。同じディレクトリを同時に二重に開くことはできない。
- エラーは `StoreError`。キー・値・TTL・時計の検査は段 1 の `KvError` をそのまま使い（`StoreError.Kv(..)`）、核が上限を超えたときは `Kv(Capacity)`、それ以外の核のエラーは種別名の `Db(..)` になる。
- 更新の成功は、核が WAL の同期に成功して commit したあとにだけ返す。**`CommitUnknown`**（WAL の書き込み・同期の失敗）が出たら成否は不明で、そのストアは停止する。以後の `get`／`put`／`delete` は `StoreError.Stopped` を返し続け（`PING` は保存データに触れないので成功のまま）、`close_persistent` して開き直す（復旧）ことでだけ戻る。永続モードの障害でメモリ専用モードへ切り替わることはない。
- 時計は注入する。期限付き PUT と期限付きキーの GET は渡されたコールバックから **UTC Unix 秒**を取る（永続モードは公開の `clock.unix_seconds()` に合わせる）。取得失敗（`ClockUnavailable`）と、同じストアでの逆行（`ClockRegressed`）、期限計算のあふれ（`DeadlineOverflow`）は型付きエラー。`now >= deadline` で期限切れ。期限切れの行は GET が同じトランザクションの中で削除してからキーなしを返すので、古い期限が新しい値を消すことはない。再起動をまたいでも期限は作り直さない。再起動をまたいで逆行しない時刻は運用の前提で、アダプターが検出するのは同じプロセス内の逆行だけ。
- 段 1 の公開型 `Ttl`、`Ack`、`KvError` を再利用し、段 1 の `stash.memory` は変更しない。キー・行の上限検査は段 1 と同じ式を `stash.persistent` に持つ。
- 試験の追加: `tests/persistent_contract.sigil`（段 1 と同じ操作契約）と `tests/persistent_recovery.sigil`（再起動後の成功済み更新と削除、期限を作り直さないこと、期限切れ後の再起動、排他ロック、`CommitUnknown` での停止と復旧、4 件の更新の全 I/O 関門を `EIO`・直前の死亡・torn write で中断して再起動後の値と期限の組を全有／全無で確認）。故障注入は核が公開している I/O 関門（`db.iohook`）だけを使う。

## TCP と Sigil クライアント（段 3）

[ADR 0059 決定 2・3](https://github.com/goodnasubi/sigil/blob/947815bfa367354dec99bd6308b8931949dec49c/docs/decisions/0059-kv-store.md) の版付き最小バイナリ TCP プロトコルと、Sigil 向けの型付きクライアント・サーバーを提供する。

- **プロトコル (`stash.protocol`)**: 各フレームは 32 bit 符号なしビッグエンディアン長、1 byte の版（初版は 1）、1 byte の操作または結果種別、本文で構成。フレーム長は版から末尾までのバイト数（2〜4096 byte）。可変長フィールドは長さ（u16 BE）とバイト列、TTL は秒の整数（i64 BE）。詳細は [docs/spec/protocol.md](docs/spec/protocol.md) の仕様表と `tests/fixtures/` の正例・負例 fixture を参照。
- **サーバー (`stash.server`)**: 既定でループバック（`127.0.0.1`）で待ち受け。接続ごとに 1 要求を直列に処理し、要求と応答は 1 対 1。応答は成功（`Success`）、値（`Value`）、キーなし（`NotFound`）、型付きエラー（`Error`）を区別する。未知の操作や引数誤りはエラー応答を返し、未対応版や 2〜4096 byte 範囲外の不正長、切断、タイムアウト（5000 ms）は接続を切断する。同時接続上限は 64。キー・値・セッション ID をログに出さない。
- **クライアント (`stash.client`)**: Sigil 製の型付きクライアント。公開操作は `connect`、`ping`、`get`、`put`、`delete`。
- **試験 (`tests/tcp.rs`)**: ループバック統合試験で正常系、分割読み取り、不正長、未対応版、キー・値の上限、同時接続、タイムアウト、切断を検証し、決定論的乱数によるパーサーのファジングを実施・記録する。

## 固定版

永続アダプターのビルドと試験は `deps/pins.env` の 2 つの完全なコミット SHA だけを使う。浮動する `main` は取得しない。

| 部品 | リポジトリ | コミット |
|---|---|---|
| コンパイラと標準ライブラリ | `goodnasubi/sigil` | `947815bfa367354dec99bd6308b8931949dec49c` |
| 核（`db/` のモジュール） | `goodnasubi/sigil-db` | `9483e36f08c35bfc62b3beeb008a531887506037` |

`cargo test` は核のチェックアウトが固定 SHA そのもので、ローカルの変更がないことを確かめる。核は `SIGIL_DB_CORE`（既定は `.deps/sigil-db-core`）で指す。コンパイラはビルドしたものを `SIGIL` で指す。更新するときは `deps/pins.env` と `.github/workflows/ci.yml` の `ref` を一緒に変え、互換性・上限・復旧試験を再実行する（CI は両者の一致を検査する）。

## 検証

Sigil の処理系と標準ライブラリ、sigil-db 核は `deps/pins.env` のコミットを使う。Cargo は Sigil のテスト起動に使用する。

```sh
scripts/fetch-deps.sh                      # .deps/ に固定コミットを取得し、一致を検査する
cargo build --release --locked --manifest-path .deps/sigil-compiler/Cargo.toml
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
SIGIL="$PWD/.deps/sigil-compiler/target/release/sigil" SIGIL_DB_CORE="$PWD/.deps/sigil-db-core" cargo test --locked
```

`cargo test` は Sigil の整形・型検査を行い、段 1 は固定時刻を用いる契約試験を、段 2 は上の契約試験と中断・復旧試験を、段 3 は TCP ループバック統合試験とパーサーファジングを、評価器とネイティブ実行の両方で検証する。
