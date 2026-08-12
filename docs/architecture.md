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
static RValue r_dispatch_setflag(RValue receiver, RValue flag, int line) {
    if (r_compiler_is(receiver, "gcc::gcc"))
        return r_lib_impl_gcc_gcc_setflag(receiver, flag, line);
    if (r_compiler_is(receiver, "clang::clang"))
        return r_lib_impl_clang_clang_setflag(receiver, flag, line);
    r_error(line, "selected compiler does not implement setflag");
}
```

したがって、Cランタイムに `setflag`、`compile`、`outputs`、`link` の意味はありません。実際のargv構築、`.o`出力先、失敗処理、リンクは `lib/gcc.roller` 等が定義します。

## Cランタイム

`RValue` はunit、integer、boolean、string、参照共有されるarray、Compilerレコード、CompilerStatusを保持するtagged unionです。短命なビルドスクリプト用に16 MiBアリーナを使用します。

ホストプリミティブは次の汎用カテゴリに限定します。

- 配列の追加、複製、連結、添字
- Compiler動的レコードのfield get/set/selection
- PATH検索
- パス変換とファイル操作
- `fork`/`execvp`/`waitpid` によるargvベースのプロセス実行
- stdout/stderrキャプチャ
- pthread bounded worker pool
- ログ、環境、時刻、制御付き終了

コンパイラやリンカを識別する分岐、専用CompileContext、`r_c_*` ビルド関数はありません。

## 並列データフロー

`for-parallel` 開始時に汎用ジョブキューを初期化します。`parallel` は `paralleable` メソッドを一度評価し、メソッド内の `sys::process::output(program, args)` を実行せずに汎用 `(program, argv)` ジョブとして収集します。全iterationの収集後、worker poolが最大 `jobs` 個を実行します。

メソッドによる出力パスの記録は収集フェーズで行われます。プロセスが失敗した場合、schedulerがエラー制御へ戻すため、後続のlinkメソッドは呼ばれません。`--dry-run` は同じ収集・argv生成を行い、プロセスを起動しません。

## エラー伝播

Lexer/Parser/TranspilerのエラーはRust側で終了コード3として報告します。生成プログラムでは `r_error` がメッセージとRoller行番号を保存して `longjmp` し、生成された `main` が終了コードへ変換します。`roller::exit(n)` は同じ制御経路で明示コードを伝えます。

外部コマンドはシェルを介さずargvを個別に `execvp` へ渡します。逐次実行はstdout/stderrを捕捉し、`.roller` 実装が `[status, stdout, stderr]` を検査します。並列実行の失敗はschedulerがプログラム名と終了状態を報告します。

## ライブラリ拡張

新しいツールチェーンは `.roller` ファイルを追加し、`compiler` フィールド、検出関数、`implement` メソッドを定義します。Zig実装は [lib/zig.roller](../lib/zig.roller) にあり、ホスト側を変更せず `zig cc` の先頭argvを追加する例です。

今後の拡張点は、型検査の強化、最適化レベルの共通contract、決定的なソース順序、ヘッダー依存解析、インクリメンタルメタデータ、POSIX以外のprocess backendです。
