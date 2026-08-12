# Roller 言語仕様

## 基本構文

ソースはUTF-8です。`//` 行コメントと、ネストしない `/* ... */` ブロックコメントを利用できます。文字列、符号なし整数、真偽値、配列をサポートします。

トップレベルには `import`、`#define`、`section`、`library` を記述できます。

```roller
import "gcc"
#define SRC "./src"

section build(jobs: int) {
    let files = dir.recursive(SRC);
}
```

文は `let`、代入、`if`/`else`、`for-parallel`、`parallel`、式文、`return` です。式にはリテラル、識別子、配列、参照 `&value`、名前空間アクセス、フィールドアクセス、呼び出し、メソッド呼び出し、添字、`!`、`==`、`!=`、`&&`、`||` があります。

## ライブラリ

```roller
library "tool" {
    function name() -> String {
        return "tool";
    }
}
```

`import "tool"` は埋め込み標準ライブラリ、スクリプト隣接の `lib/tool.roller`、または作業ディレクトリの `lib/tool.roller` を読み込みます。呼び出しは `tool::name()` です。

## Compiler 契約と具体実装

`Compiler` はコアが提供する契約です。`Compiler::new()` は未選択オブジェクト、`Compiler::AVAILABLE` と `Compiler::UNAVAILABLE`/`NOTFOUND` は検出結果を表します。具体的なデータと処理はライブラリに置きます。

```roller
library "gcc" {
    compiler gcc {
        flags: Vec<String>,
        file_location: String,
        objects: Vec<String>,
        available: bool,
        failure: bool,
    }

    function get_compiler(compiler: Compiler) -> CompilerStatus {
        if sys::cmd::is_exists("gcc") {
            compiler = self::gcc;
            compiler.file_location = sys::cmd::which("gcc");
            compiler.available = true;
            return Compiler::AVAILABLE;
        }
        return Compiler::NOTFOUND;
    }

    implement Self::gcc {
        function setflag(compiler: self, flag: String) -> Compiler {
            compiler.flags.push(flag);
            return compiler;
        }

        paralleable function compile(compiler: self, file: String) {
            // argvを組み立て、sys::process::outputで実行する
        }
    }
}
```

`compiler = self::gcc` は、呼出元の未選択Compilerを具体実装へ切り替えます。フィールド代入は `compiler.field = value`、参照は `compiler.field` です。`implement` の先頭引数がメソッドレシーバです。

フィールド型、同名メソッドの引数型・引数個数・戻り値型は具体実装ごとに独立しています。呼び出し時は選択済み実装のシグネチャで引数個数を検証し、値の具体的な型は操作時に検証します。複数実装で戻り値型が一致しない呼び出しは、トランスパイラ内では動的な値として扱われます。これにより、たとえば `gcc.optimize(integer)` と `zig.optimize(String)`、あるいはツールチェーン固有の `compile` 入力を同じコアへ追加できます。

型名は名前空間修飾と再帰的なジェネリックを保持できます（例: `Map<String, Vec<tool::Input>>`）。フィールドの初期値は `String` が空文字列、`integer` が0、`bool` がfalse、`Vec<T>` が空配列で、それ以外はunitです。

## 配列と文字列

- `array.push(value)` / `push_str(value)`: 1要素追加
- `array.push_vec(other)`: 配列を末尾へ追加
- `array.copy()`: 独立した配列コンテナを作成
- `array.is_empty()`, `string.is_empty()`
- `array.join(separator)`
- `array[index]`

## 汎用 `sys::` API

コンパイラ固有APIはありません。主要な汎用APIは次のとおりです。

- `sys::cmd::which(name)`, `sys::cmd::is_exists(name)`
- `sys::process::output(program, args)` → `[exit_code, stdout, stderr]`
- `sys::process::run/status/spawn/wait/kill`
- `sys::path::join(base, child)`, `sys::path::replace_extension(path, ext)`, `sys::path::extension(path)`
- `sys::fs::mkdir_parent(path)`, `mkdir_all`, `read`, `write`, `exists` など
- `sys::str::concat(a, b)`, `contains(a, b)`
- `sys::env::*`, `sys::io::*`, `sys::time::*`

`sys::process::output` のargsは文字列配列であり、シェル文字列へ再結合されません。

## 並列実行

```roller
for-parallel file in dir.recursive(SRC) {
    parallel compiler.compile(file);
}
```

`dir.recursive` は拡張子を限定せず、通常ファイルを決定的なパス順で返します。上の `file` は固定されたCソース型ではなく動的なループ要素です。各Compiler実装が `sys::path::extension` などで受理する入力を選択できます。

`parallel` は `paralleable` と宣言されたCompilerメソッドを受け付けます。メソッド名は `compile` に固定されず、引数は通常の動的ディスパッチと同じく選択済み実装のシグネチャで検証されます。メソッド内の汎用プロセス実行をジョブとして収集し、ループ後に設定上限内で実行します。失敗時は新規ジョブ開始を止め、リンクへ進みません。

## 組み込み名前空間

- `log::info`, `log::error` / `log::err`
- `roller::set_parallel_jobs`, `roller::exit`
- `dir.recursive`
- `process::run`

構文エラーは元ファイルの行・列・spanを表示します。生成Cには `#line` を入れ、実行時エラーも元のRoller行番号を報告します。
