# プロトコル Fixture 一覧

`docs/spec/protocol.md` に定義された版付きバイナリプロトコルの正例・負例 fixture。

## 正例 (Valid Fixtures)
- `req_ping.bin`: PING 要求（長さ 2、版 1、Op 1）
- `res_success.bin`: Success 応答（長さ 2、版 1、Result 1）
- `req_get.bin`: GET 要求（キー "abc"）
- `res_value.bin`: Value 応答（値 "world"）
- `res_not_found.bin`: NotFound 応答（キーなし）
- `req_put_no_ttl.bin`: PUT 要求（キー "k"、値 "v1"、期限なし `ttl_tag=0`）
- `req_put_ttl.bin`: PUT 要求（キー "k"、値 "v1"、TTL 60秒 `ttl_tag=1`）
- `req_delete.bin`: DELETE 要求（キー "abc"）
- `res_error.bin`: Error 応答（エラーコード 3 `KeyLength`、メッセージ "bad"）
- `req_max_frame.bin`: 最大許容フレーム長（4096 バイト）の PUT 要求境界値

## 負例 (Invalid Fixtures)
- `err_length_too_small.bin`: フレーム長が 2 未満（長さ 1）
- `err_length_too_large.bin`: フレーム長が 4096 超過（長さ 4097）
- `err_unsupported_version.bin`: 未対応版（Version 2）
- `err_unknown_op.bin`: 未知の操作種別（Op 99）
- `err_truncated_payload.bin`: フレーム長 10 に対して本文が途中で途切れた電文
- `err_invalid_ttl_tag.bin`: 不正な TTL タグ（`ttl_tag=99`）
