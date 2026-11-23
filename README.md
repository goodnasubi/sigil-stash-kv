# sigil-stash-kv

[ADR 0059](https://github.com/goodnasubi/sigil/blob/947815bfa367354dec99bd6308b8931949dec49c/docs/decisions/0059-kv-store.md) 段 1 のメモリ専用 KV。公開操作は `stash.memory` の `get`、`put`、`delete`、`ping`。値はバイト列で、GET は `Result<Option<Bytes>, KvError>`、更新と PING は `Result<Ack, KvError>` を返す。

`new_memory(capacity, require_ttl)` で最大エントリー数と TTL 必須の有無を指定する。キャッシュ・セッション用途では `require_ttl` を有効にする。期限切れのキーへの GET はその枠を解放する。満杯時は既存キーの置換と空き枠の再利用を許し、新規キーを収容できなければ `KvError.Capacity` を返す。

キーは 1〜512 byte、値は 0〜3072 byte。固定参照版の [sigil-db の型符号化](https://github.com/goodnasubi/sigil-db/blob/9483e36f08c35bfc62b3beeb008a531887506037/db/types.sigil) に従い、キーは `5 + key_len`、行は `2 + (5 + key_len) + (5 + value_len) + 9` で符号化後サイズを検査する。行サイズはキー、値、期限を 1 列ずつ持つ前提で、期限はタグ付き Int 一つ分を計上する。公開上限での符号化後サイズはそれぞれ 517、3605 byte で、核の 1024、4096 byte 上限に収まる。

## 時計の契約

期限付き PUT と期限付きエントリーの GET は、渡されたコールバックから単調時刻を取得する。同じストアへの操作には同じ起点の非負ナノ秒を返す時計を使う。TTL の単位は正の整数秒。時刻取得の失敗、逆行、期限計算のオーバーフローは型付きエラーになる。時計取得から期限判定、値と期限の置換、失効時の削除まではストアの排他的な可変借用内で行う。

Sigil の ADR 参照版 `0c1c441e60933fadbe05e1c5c63b11bcf4ad4409` と ADR マージ版 `947815bfa367354dec99bd6308b8931949dec49c` を確認した。公開の `clock.unix_seconds()` は永続モード向けの UTC Unix 秒を返す。内部の `std.os.io.io_now()` はスケジューラ用で、利用者コードから公開時計として利用できず、内部の時計読み取りも取得失敗を値として返さない。この段の実装は時計を注入して検証する。実運用の時計アダプターには、取得失敗を型で返し、同一起点の非負ナノ秒を公開する Sigil 側の契約が必要になる。評価器での時刻・失敗注入とネイティブ側での単調時計取得を、その契約で扱えることが必要になる。

## 検証

Sigil の処理系と標準ライブラリは次のコミットを使う。Cargo は Sigil のテスト起動に使用する。

```sh
git clone https://github.com/goodnasubi/sigil.git /tmp/sigil-stash-kv-compiler
git -C /tmp/sigil-stash-kv-compiler checkout --detach 947815bfa367354dec99bd6308b8931949dec49c
cargo build --locked --manifest-path /tmp/sigil-stash-kv-compiler/Cargo.toml
cargo fmt --check
cargo clippy --all-targets -- -D warnings
SIGIL=/tmp/sigil-stash-kv-compiler/target/debug/sigil cargo test --locked
```

`cargo test` は Sigil の整形・型検査を行い、固定時刻を用いる同じ契約試験を評価器とネイティブ実行で検証する。
