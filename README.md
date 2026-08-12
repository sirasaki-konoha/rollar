# Roller

Roller は C プロジェクト向けのビルド記述言語です。Roller スクリプトを C にトランスパイルし、TCC、GCC、または Clang で生成コードを実行します。

現在の実行系は C トランスパイラのみです。旧Rustツリーウォーカーインタプリタと旧Rustビルドバックエンドは削除されています。

## ビルド

Rust 1.85 以上が必要です。

```sh
cargo build --workspace
cargo install --path crates/roller-cli
```

## CLI

```text
roller [OPTIONS] [SCRIPT] <SECTION>
```

デフォルトのスクリプトは `build.roller` です。

```sh
roller build
roller build --jobs 8
roller path/to/build.roller build --verbose
roller build --dry-run
roller path/to/build.roller build --check
roller clean
roller run
```

`--jobs` は最大並列数、`--verbose` は外部コマンドのargv表示、`--dry-run` は外部プロセスを起動しない計画表示です。`clean` の組み込みfallbackはプロジェクト内の `.roller/build` だけを正規化・検証して削除します。

## コンパイラライブラリ

コンパイラ固有の状態と処理はRustやCランタイムへ埋め込まず、[lib/gcc.roller](lib/gcc.roller)、[lib/clang.roller](lib/clang.roller)、[lib/zig.roller](lib/zig.roller) に記述します。

```roller
library "gcc" {
    compiler gcc {
        flags: Vec<String>,
        output: String,
        gcc_available: bool,
        file_location: String,
        objects: Vec<String>,
        optimize: integer,
        failure: bool,
    }

    function get_compiler(compiler: Compiler) -> CompilerStatus {
        if sys::cmd::is_exists("gcc") {
            compiler = self::gcc;
            compiler.file_location = sys::cmd::which("gcc");
            compiler.gcc_available = true;
            return Compiler::AVAILABLE;
        }
        return Compiler::NOTFOUND;
    }

    implement Self::gcc {
        function setflag(compiler: self, flag: String) -> Compiler {
            compiler.flags.push(flag);
            return compiler;
        }
    }
}
```

`Compiler::new()` は未選択のコア契約オブジェクトを作ります。`get_compiler` が具体実装を選択すると、`cc.setflag(...)` などは実装名に基づいて `.roller` の `implement` メソッドへ動的ディスパッチされます。

Cランタイムが提供するのは、動的フィールド、配列、パス操作、ファイル操作、argvベースのプロセス実行、bounded並列ジョブ実行などの汎用機構です。`setflag`、`compile`、`outputs`、`link` を実装する専用ホスト関数はありません。

## サンプル

```sh
cargo run -p roller-cli -- examples/hello-c/build.roller build --jobs 4
./examples/hello-c/myproject
cargo run -p roller-cli -- examples/hello-c/build.roller run
cargo run -p roller-cli -- examples/hello-c/build.roller clean
```

成功時は `Hello from Roller!` と表示されます。

## 構成

- `roller-cli`: CLI、ソース読込、生成Cのコンパイル・実行、安全なclean
- `roller-parser`: Lexer、AST、再帰下降Parser
- `roller-transpiler`: ASTからCへの変換、ライブラリ読込、動的メソッドディスパッチ
- `roller-diagnostics`: 共通のソース読込診断
- `lib/*.roller`: GCC、Clang、Zigの具体的なコンパイラ実装

詳細は [言語仕様](docs/language.md) と [アーキテクチャ](docs/architecture.md) を参照してください。

## 検証

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

## 現在の制限と安全性

- Linux/macOS向けのPOSIX実行系です。
- `.c` の再帰探索を対象とし、ヘッダー依存解析とインクリメンタルメタデータは未実装です。
- `compiler` フィールド型は現在 `String`、`integer`、`bool`、`Vec<T>` を初期化できます。
- `optimize(level)` は符号なし整数の0〜3を受け付け、各ライブラリが `-O0`〜`-O3` をargvへ追加します。
- Rollerスクリプトは任意の外部コマンドを実行できます。信頼できるビルドスクリプトだけを実行してください。argvはシェルを介さず `execvp` へ個別に渡されます。
