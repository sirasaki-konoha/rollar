# Roller アーキテクチャ

## 実行フロー

```text
build.roller
  → Lexer → AST → 再帰下降Parser
  → importされた .roller ライブラリを解析
  → Cトランスパイラ
  → .roller/build_script.c + .roller/roller-runtime.h
  → TCC -run、またはGCC/Clangで生成Cをコンパイル
  → 指定sectionを実行
```

Rustインタプリタは存在しません。実行意味論は生成CとCランタイムの経路に一本化されています。

## クレート

- `roller-cli`: 引数処理、ソース読込、生成物配置、Cコンパイラ選択、実行、safe-clean
- `roller-parser`: token/span、AST、Lexer、Parser
- `roller-transpiler`: import解決、簡易型追跡、Cコード生成
- `roller-diagnostics`: 共通ソース診断

旧 `roller-runtime` Rustインタプリタと旧 `roller-build` Rustビルドモデルは削除済みです。`roller-runtime.h` は生成プログラム用のCランタイムであり、Rustインタプリタとは別物です。

## Compiler の分離

コアが知るのは以下だけです。

- 未選択 `Compiler` の生成
- concrete implementation名
- 名前付き動的フィールド
- `AVAILABLE` / `UNAVAILABLE` ステータス
- implementation名に基づくメソッドディスパッチ

トランスパイラは各 `compiler` 宣言からコンストラクタを生成し、各 `implement` メソッドを通常のC関数へ変換します。同名メソッドについて次のようなdispatcherを生成します。

```c
static RValue r_dispatch_setflag(RValue receiver, RValue arguments, int line) {
    if (r_compiler_is(receiver, "gcc::gcc")) {
        r_array_require_length(arguments, 1, "setflag", line);
        return r_lib_impl_gcc_gcc_setflag(
            receiver, r_array_at(arguments, 0, line), line);
    }
    /* 実装ごとに別の引数個数・型を持てる */
    r_error(line, "selected compiler does not implement setflag");
}
```

dispatcherは引数を動的配列で受け、選択された実装自身のarityを検証します。同名フィールドの型や同名メソッドのシグネチャを全実装で統合しないため、実装の登録順にも依存しません。複数実装で戻り値型が一致する場合だけ静的な型情報を引き継ぎ、一致しなければ動的値として扱います。

したがって、Cランタイムに `setflag`、`compile`、`outputs`、`link` の意味はありません。実際の入力選択、argv構築、出力形式、失敗処理、リンクは `lib/gcc.roller` 等が定義します。

## Cランタイム

`RValue` はunit、integer、boolean、string、参照共有されるarray、Compilerレコード、CompilerStatusを保持するtagged unionです。短命なビルドスクリプト用に16 MiBアリーナを使用します。

ホストプリミティブは次の汎用カテゴリに限定します。

- 配列の追加、複製、連結、添字
- Compiler動的レコードのfield get/set/selection
- PATH検索
- 拡張子を限定しないファイル列挙、パス変換とファイル操作
- `fork`/`execvp`/`waitpid` によるargvベースのプロセス実行
- stdout/stderrキャプチャ
- pthread bounded worker pool
- ログ、環境、時刻、制御付き終了

コンパイラやリンカを識別する分岐、専用CompileContext、`r_c_*` ビルド関数はありません。

## 並列データフロー

`for-parallel` 開始時に汎用ジョブキューを初期化します。iteration要素はファイル専用型ではなく任意の `RValue` です。`parallel` は名前を固定しない `paralleable` Compilerメソッドを一度評価し、メソッド内の `sys::process::output(program, args)` を実行せずに汎用 `(program, argv)` ジョブとして収集します。全iterationの収集後、worker poolが最大 `jobs` 個を実行します。

メソッドによる出力パスの記録は収集フェーズで行われます。プロセスが失敗した場合、schedulerがエラー制御へ戻すため、後続のlinkメソッドは呼ばれません。`--dry-run` は同じ収集・argv生成を行い、プロセスを起動しません。

## エラー伝播

Lexer/Parser/TranspilerのエラーはRust側で終了コード3として報告します。生成プログラムでは `r_error` がメッセージとRoller行番号を保存して `longjmp` し、生成された `main` が終了コードへ変換します。`roller::exit(n)` は同じ制御経路で明示コードを伝えます。

外部コマンドはシェルを介さずargvを個別に `execvp` へ渡します。逐次実行はstdout/stderrを捕捉し、`.roller` 実装が `[status, stdout, stderr]` を検査します。並列実行の失敗はschedulerがプログラム名と終了状態を報告します。

## ライブラリ拡張

新しいツールチェーンは `.roller` ファイルを追加し、`compiler` フィールド、検出関数、`implement` メソッドを定義します。Zig実装は [lib/zig.roller](../lib/zig.roller) にあり、GCC/Clangとは異なる文字列の最適化モードを持ち、ホスト側を変更せずネイティブの `zig build-obj` / `zig build-exe` を使う例です。

今後の拡張点は、実装選択を考慮した型検査の強化、ヘッダー依存解析、インクリメンタルメタデータ、POSIX以外のprocess backendです。
