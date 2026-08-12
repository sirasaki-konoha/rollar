/*
 * Roller C Runtime — roller-runtime.h
 *
 * Minimal C99 + POSIX runtime providing ONLY generic OS primitives.
 * Language-specific operations (compilation, linking, etc.) are
 * implemented in Roller library files using sys::process_run etc.
 */

#ifndef ROLLER_RUNTIME_H
#define ROLLER_RUNTIME_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdarg.h>
#include <setjmp.h>
#include <dirent.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <errno.h>
#include <time.h>
#include <pthread.h>

/* ========================================================================
 * Error handling
 * ======================================================================== */

static jmp_buf r_error_jmp;
static char    r_error_msg[4096];
static int     r_error_line;

static void r_error(int line, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void r_error(int line, const char *fmt, ...) {
    r_error_line = line;
    va_list args;
    va_start(args, fmt);
    vsnprintf(r_error_msg, sizeof(r_error_msg), fmt, args);
    va_end(args);
    longjmp(r_error_jmp, 1);
}

/* ========================================================================
 * Arena allocator
 * ======================================================================== */

#define R_ARENA_SIZE (16 * 1024 * 1024)

static char  *r_arena_buf   = NULL;
static size_t r_arena_used  = 0;
static size_t r_arena_total = 0;

static void r_arena_init(void) {
    if (r_arena_buf) return;
    r_arena_buf = (char*)malloc(R_ARENA_SIZE);
    if (!r_arena_buf) { fprintf(stderr, "roller: arena alloc failed\n"); exit(1); }
    r_arena_total = R_ARENA_SIZE;
    r_arena_used  = 0;
}

static void *r_arena_alloc(size_t size) {
    if (!r_arena_buf) r_arena_init();
    size = (size + 7) & ~(size_t)7;
    if (r_arena_used + size > r_arena_total) {
        fprintf(stderr, "roller: arena exhausted\n"); exit(1);
    }
    void *ptr = r_arena_buf + r_arena_used;
    r_arena_used += size;
    return ptr;
}

static char *r_arena_strdup(const char *s) {
    if (!s) return NULL;
    size_t len = strlen(s) + 1;
    char *copy = (char*)r_arena_alloc(len);
    memcpy(copy, s, len);
    return copy;
}

static void r_arena_reset(void) { r_arena_used = 0; }
static void r_arena_destroy(void) {
    free(r_arena_buf);
    r_arena_buf = NULL; r_arena_used = 0; r_arena_total = 0;
}

/* ========================================================================
 * Value types — generic tagged union
 * ======================================================================== */

typedef enum {
    R_TYPE_ERROR = -1,
    R_TYPE_UNIT = 0,
    R_TYPE_INTEGER, R_TYPE_BOOLEAN, R_TYPE_STRING, R_TYPE_ARRAY,
    R_TYPE_COMPILER, R_TYPE_COMPILER_STATUS,
} RType;

typedef struct RValue RValue;
typedef struct { RValue *elements; size_t count, capacity; } RArray;
typedef struct RCompilerField RCompilerField;

struct RValue {
    RType type;
    union {
        uint64_t  integer;
        int       boolean;
        char     *string;
        RArray   *array;
        struct RCompiler *compiler;
        int compiler_status; /* 1=available, 0=unavailable */
    } as;
};

/* Compiler is a dynamic record selected from a Roller `compiler` declaration. */
typedef struct RCompiler {
    char *implementation;
    RCompilerField *fields;
    size_t field_count, field_capacity;
} RCompiler;

struct RCompilerField { char *name; RValue value; };

static const char *r_value_type_name(RValue v);

/* ========================================================================
 * Global state
 * ======================================================================== */

static int  r_parallel_jobs = 1;
static char r_project_root[4096] = ".";
static int  r_dry_run = 0;
static int  r_verbose = 0;

/* ========================================================================
 * Value constructors
 * ======================================================================== */

static RValue r_unit(void) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_UNIT; return v;
}
static RValue r_integer(uint64_t n) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_INTEGER; v.as.integer = n; return v;
}
static RValue r_boolean(int b) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_BOOLEAN; v.as.boolean = b; return v;
}
static RValue r_string(const char *s) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_STRING;
    v.as.string = r_arena_strdup(s ? s : ""); return v;
}
static RValue r_array_new(void) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_ARRAY;
    v.as.array = (RArray*)r_arena_alloc(sizeof(RArray));
    memset(v.as.array, 0, sizeof(RArray));
    return v;
}
static void r_array_push(RValue *arr, RValue elem) {
    if (arr->type != R_TYPE_ARRAY) return;
    if (arr->as.array->count >= arr->as.array->capacity) {
        size_t cap = arr->as.array->capacity == 0 ? 8 : arr->as.array->capacity * 2;
        RValue *e = (RValue*)r_arena_alloc(cap * sizeof(RValue));
        if (arr->as.array->elements) memcpy(e, arr->as.array->elements, arr->as.array->count * sizeof(RValue));
        arr->as.array->elements = e; arr->as.array->capacity = cap;
    }
    arr->as.array->elements[arr->as.array->count++] = elem;
}
static RValue r_array_push_value(RValue arr, RValue elem, int line) {
    if (arr.type != R_TYPE_ARRAY)
        r_error(line, "push requires array, got %s", r_value_type_name(arr));
    r_array_push(&arr, elem);
    return r_unit();
}
static RValue r_array_extend(RValue destination, RValue source, int line) {
    if (destination.type != R_TYPE_ARRAY || source.type != R_TYPE_ARRAY)
        r_error(line, "push_vec requires two arrays");
    for (size_t i = 0; i < source.as.array->count; i++)
        r_array_push(&destination, source.as.array->elements[i]);
    return r_unit();
}
static RValue r_compiler_status_available(void) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_COMPILER_STATUS;
    v.as.compiler_status = 1; return v;
}
static RValue r_compiler_status_unavailable(void) {
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_COMPILER_STATUS;
    v.as.compiler_status = 0; return v;
}
static RValue r_compiler_new(void) {
    RCompiler *c = (RCompiler*)r_arena_alloc(sizeof(RCompiler));
    memset(c, 0, sizeof(RCompiler));
    RValue v; memset(&v, 0, sizeof(v)); v.type = R_TYPE_COMPILER;
    v.as.compiler = c; return v;
}

static RValue r_compiler_instance(const char *implementation) {
    RValue compiler = r_compiler_new();
    compiler.as.compiler->implementation = r_arena_strdup(implementation);
    return compiler;
}

static int r_compiler_is(RValue compiler, const char *implementation) {
    return compiler.type == R_TYPE_COMPILER && compiler.as.compiler->implementation &&
           strcmp(compiler.as.compiler->implementation, implementation) == 0;
}

static const char *r_compiler_implementation(RValue compiler, int line) {
    if (compiler.type != R_TYPE_COMPILER)
        r_error(line, "expected Compiler, got %s", r_value_type_name(compiler));
    return compiler.as.compiler->implementation ? compiler.as.compiler->implementation : "<unselected>";
}

static RCompilerField *r_compiler_find_field(RCompiler *compiler, const char *name) {
    for (size_t i = 0; i < compiler->field_count; i++)
        if (strcmp(compiler->fields[i].name, name) == 0) return &compiler->fields[i];
    return NULL;
}

static void r_compiler_set(RValue compiler, const char *name, RValue value, int line) {
    if (compiler.type != R_TYPE_COMPILER)
        r_error(line, "field assignment requires Compiler, got %s", r_value_type_name(compiler));
    RCompilerField *field = r_compiler_find_field(compiler.as.compiler, name);
    if (field) { field->value = value; return; }
    RCompiler *record = compiler.as.compiler;
    if (record->field_count >= record->field_capacity) {
        size_t capacity = record->field_capacity == 0 ? 8 : record->field_capacity * 2;
        RCompilerField *fields = (RCompilerField*)r_arena_alloc(capacity * sizeof(RCompilerField));
        if (record->fields)
            memcpy(fields, record->fields, record->field_count * sizeof(RCompilerField));
        record->fields = fields;
        record->field_capacity = capacity;
    }
    record->fields[record->field_count].name = r_arena_strdup(name);
    record->fields[record->field_count].value = value;
    record->field_count++;
}

static RValue r_compiler_get(RValue compiler, const char *name, int line) {
    if (compiler.type != R_TYPE_COMPILER)
        r_error(line, "field access requires Compiler, got %s", r_value_type_name(compiler));
    RCompilerField *field = r_compiler_find_field(compiler.as.compiler, name);
    if (!field) r_error(line, "compiler implementation '%s' has no field '%s'",
                        r_compiler_implementation(compiler, line), name);
    return field->value;
}

static void r_compiler_assign(RValue destination, RValue source, int line) {
    if (destination.type != R_TYPE_COMPILER || source.type != R_TYPE_COMPILER)
        r_error(line, "compiler selection requires two Compiler values");
    destination.as.compiler->implementation = source.as.compiler->implementation;
    destination.as.compiler->fields = source.as.compiler->fields;
    destination.as.compiler->field_count = source.as.compiler->field_count;
    destination.as.compiler->field_capacity = source.as.compiler->field_capacity;
}

/* ========================================================================
 * Value operations
 * ======================================================================== */

static const char *r_value_type_name(RValue v) {
    switch (v.type) {
        case R_TYPE_ERROR: return "error"; case R_TYPE_UNIT: return "unit";
        case R_TYPE_INTEGER: return "int"; case R_TYPE_BOOLEAN: return "bool";
        case R_TYPE_STRING: return "string"; case R_TYPE_ARRAY: return "array";
        case R_TYPE_COMPILER: return "Compiler";
        case R_TYPE_COMPILER_STATUS: return "CompilerStatus";
    }
    return "unknown";
}
static int r_value_truthy(RValue v, int line) {
    switch (v.type) {
        case R_TYPE_BOOLEAN: return v.as.boolean;
        case R_TYPE_INTEGER: return v.as.integer != 0;
        case R_TYPE_COMPILER_STATUS: return v.as.compiler_status != 0;
        default: r_error(line, "cannot use %s as condition", r_value_type_name(v)); return 0;
    }
}
static int r_value_equal(RValue a, RValue b, int line) {
    if (a.type != b.type) {
        if (a.type == R_TYPE_COMPILER_STATUS && b.type == R_TYPE_COMPILER_STATUS)
            return a.as.compiler_status == b.as.compiler_status;
        r_error(line, "cannot compare %s and %s", r_value_type_name(a), r_value_type_name(b));
        return 0;
    }
    switch (a.type) {
        case R_TYPE_UNIT: return 1;
        case R_TYPE_INTEGER: return a.as.integer == b.as.integer;
        case R_TYPE_BOOLEAN: return a.as.boolean == b.as.boolean;
        case R_TYPE_STRING: return strcmp(a.as.string, b.as.string) == 0;
        case R_TYPE_COMPILER_STATUS: return a.as.compiler_status == b.as.compiler_status;
        default: r_error(line, "cannot compare %s", r_value_type_name(a)); return 0;
    }
}
static RValue r_value_eq(RValue a, RValue b, int line)  { return r_boolean(r_value_equal(a, b, line)); }
static RValue r_value_neq(RValue a, RValue b, int line) { return r_boolean(!r_value_equal(a, b, line)); }
static RValue r_value_and(RValue a, RValue b, int line) {
    if (a.type != R_TYPE_BOOLEAN) r_error(line, "&& requires bool, got %s", r_value_type_name(a));
    if (!a.as.boolean) return r_boolean(0);
    if (b.type != R_TYPE_BOOLEAN) r_error(line, "&& requires bool, got %s", r_value_type_name(b));
    return b;
}
static RValue r_value_or(RValue a, RValue b, int line) {
    if (a.type != R_TYPE_BOOLEAN) r_error(line, "|| requires bool, got %s", r_value_type_name(a));
    if (a.as.boolean) return r_boolean(1);
    if (b.type != R_TYPE_BOOLEAN) r_error(line, "|| requires bool, got %s", r_value_type_name(b));
    return b;
}
static RValue r_value_not(RValue v, int line) {
    if (v.type != R_TYPE_BOOLEAN) r_error(line, "! requires bool, got %s", r_value_type_name(v));
    return r_boolean(!v.as.boolean);
}
static RValue r_array_get(RValue arr, RValue idx, int line) {
    if (arr.type != R_TYPE_ARRAY) r_error(line, "index requires array, got %s", r_value_type_name(arr));
    if (idx.type != R_TYPE_INTEGER) r_error(line, "index requires int, got %s", r_value_type_name(idx));
    size_t i = (size_t)idx.as.integer;
    if (i >= arr.as.array->count) r_error(line, "index %zu out of bounds (len %zu)", i, arr.as.array->count);
    return arr.as.array->elements[i];
}
static RValue r_array_at(RValue arr, size_t index, int line) {
    if (arr.type != R_TYPE_ARRAY)
        r_error(line, "method arguments must be an array, got %s", r_value_type_name(arr));
    if (index >= arr.as.array->count)
        r_error(line, "method argument %zu is out of bounds (len %zu)", index, arr.as.array->count);
    return arr.as.array->elements[index];
}
static void r_array_require_length(RValue arr, size_t expected, const char *method, int line) {
    if (arr.type != R_TYPE_ARRAY)
        r_error(line, "arguments for method %s must be an array", method);
    if (arr.as.array->count != expected)
        r_error(line, "method %s expects %zu argument(s), got %zu",
                method, expected, arr.as.array->count);
}
static RValue r_array_len(RValue arr, int line) {
    if (arr.type != R_TYPE_ARRAY) r_error(line, "len requires array, got %s", r_value_type_name(arr));
    return r_integer((uint64_t)arr.as.array->count);
}
static RValue r_array_copy(RValue arr, int line) {
    if (arr.type != R_TYPE_ARRAY) r_error(line, "copy requires array, got %s", r_value_type_name(arr));
    RValue copy = r_array_new();
    for (size_t i = 0; i < arr.as.array->count; i++)
        r_array_push(&copy, arr.as.array->elements[i]);
    return copy;
}
static RValue r_value_is_empty(RValue value, int line) {
    if (value.type == R_TYPE_ARRAY) return r_boolean(value.as.array->count == 0);
    if (value.type == R_TYPE_STRING) return r_boolean(value.as.string[0] == '\0');
    r_error(line, "is_empty requires array or string, got %s", r_value_type_name(value));
    return r_boolean(0);
}
static RValue r_array_join(RValue array, RValue separator, int line) {
    if (array.type != R_TYPE_ARRAY || separator.type != R_TYPE_STRING)
        r_error(line, "join requires an array and a string separator");
    size_t length = 1;
    for (size_t i = 0; i < array.as.array->count; i++) {
        if (array.as.array->elements[i].type != R_TYPE_STRING)
            r_error(line, "join requires an array of strings");
        length += strlen(array.as.array->elements[i].as.string);
        if (i > 0) length += strlen(separator.as.string);
    }
    char *result = (char*)r_arena_alloc(length);
    result[0] = '\0';
    for (size_t i = 0; i < array.as.array->count; i++) {
        if (i > 0) strcat(result, separator.as.string);
        strcat(result, array.as.array->elements[i].as.string);
    }
    return r_string(result);
}

/* ========================================================================
 * Logging and control
 * ======================================================================== */

static void r_log_info(const char *msg)  { printf("%s\n", msg); }
static void r_log_error(const char *msg) { fprintf(stderr, "%s\n", msg); }
static void r_exit_impl(int code) {
    r_error_line = 0;
    snprintf(r_error_msg, sizeof(r_error_msg), "__exit__%d", code);
    longjmp(r_error_jmp, 1);
}

/* ========================================================================
 * sys:: primitives — generic OS operations only
 * ======================================================================== */

/* --- PATH search --- */
static RValue r_sys_find_executable(RValue name, int line) {
    if (name.type != R_TYPE_STRING)
        r_error(line, "sys::find_executable requires string, got %s", r_value_type_name(name));
    const char *path_env = getenv("PATH");
    if (!path_env) return r_string("");
    char *copy = r_arena_strdup(path_env);
    char *save = NULL;
    char *dir = strtok_r(copy, ":", &save);
    static char cand[4096];
    while (dir) {
        snprintf(cand, sizeof(cand), "%s/%s", dir, name.as.string);
        if (access(cand, X_OK) == 0) return r_string(cand);
        dir = strtok_r(NULL, ":", &save);
    }
    return r_string("");
}

static RValue r_sys_cmd_which(RValue name, int line) {
    return r_sys_find_executable(name, line);
}

static RValue r_sys_cmd_is_exists(RValue name, int line) {
    RValue path = r_sys_find_executable(name, line);
    return r_boolean(path.as.string[0] != '\0');
}

/* --- String operations --- */
static RValue r_sys_str_concat(RValue a, RValue b, int line) {
    if (a.type != R_TYPE_STRING || b.type != R_TYPE_STRING)
        r_error(line, "str::concat requires two strings");
    size_t la = strlen(a.as.string), lb = strlen(b.as.string);
    char *buf = (char*)r_arena_alloc(la + lb + 1);
    memcpy(buf, a.as.string, la);
    memcpy(buf + la, b.as.string, lb + 1);
    return r_string(buf);
}

static RValue r_sys_str_contains(RValue haystack, RValue needle, int line) {
    if (haystack.type != R_TYPE_STRING || needle.type != R_TYPE_STRING)
        r_error(line, "str::contains requires two strings");
    return r_boolean(strstr(haystack.as.string, needle.as.string) != NULL);
}

/* --- Path operations --- */
static RValue r_sys_path_join(RValue base, RValue child, int line) {
    if (base.type != R_TYPE_STRING || child.type != R_TYPE_STRING)
        r_error(line, "sys::path::join requires two strings");
    const char *relative = child.as.string;
    while (relative[0] == '.' && relative[1] == '/') relative += 2;
    size_t base_len = strlen(base.as.string), child_len = strlen(relative);
    int needs_slash = base_len > 0 && base.as.string[base_len - 1] != '/';
    char *result = (char*)r_arena_alloc(base_len + child_len + (size_t)needs_slash + 1);
    snprintf(result, base_len + child_len + (size_t)needs_slash + 1, "%s%s%s",
             base.as.string, needs_slash ? "/" : "", relative);
    return r_string(result);
}

static RValue r_sys_path_replace_extension(RValue path, RValue extension, int line) {
    if (path.type != R_TYPE_STRING || extension.type != R_TYPE_STRING)
        r_error(line, "sys::path::replace_extension requires two strings");
    const char *slash = strrchr(path.as.string, '/');
    const char *dot = strrchr(path.as.string, '.');
    size_t stem_len = dot && (!slash || dot > slash) ? (size_t)(dot - path.as.string) : strlen(path.as.string);
    const char *ext = extension.as.string;
    while (*ext == '.') ext++;
    size_t ext_len = strlen(ext);
    char *result = (char*)r_arena_alloc(stem_len + ext_len + 2);
    snprintf(result, stem_len + ext_len + 2, "%.*s.%s", (int)stem_len, path.as.string, ext);
    return r_string(result);
}

static RValue r_sys_path_extension(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::path::extension requires a string path");
    const char *slash = strrchr(path.as.string, '/');
    const char *dot = strrchr(path.as.string, '.');
    if (!dot || (slash && dot < slash) || dot[1] == '\0') return r_string("");
    return r_string(dot + 1);
}

/* --- Directory traversal --- */
static void r_sys_collect_files(const char *dir_path, RValue *result) {
    DIR *dir = opendir(dir_path);
    if (!dir) return;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (entry->d_name[0] == '.') continue;
        char full[4096]; snprintf(full, sizeof(full), "%s/%s", dir_path, entry->d_name);
        struct stat st; if (stat(full, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) r_sys_collect_files(full, result);
        else if (S_ISREG(st.st_mode)) r_array_push(result, r_string(full));
    }
    closedir(dir);
}

static int r_sys_compare_paths(const void *left, const void *right) {
    const RValue *left_value = (const RValue*)left;
    const RValue *right_value = (const RValue*)right;
    return strcmp(left_value->as.string, right_value->as.string);
}

static RValue r_sys_dir_recursive(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::dir_recursive requires string, got %s", r_value_type_name(path));
    struct stat st;
    if (stat(path.as.string, &st) != 0) r_error(line, "directory not found: %s", path.as.string);
    if (!S_ISDIR(st.st_mode)) r_error(line, "not a directory: %s", path.as.string);
    RValue result = r_array_new();
    r_sys_collect_files(path.as.string, &result);
    qsort(result.as.array->elements, result.as.array->count, sizeof(RValue), r_sys_compare_paths);
    return result;
}

/* --- Filesystem operations --- */
static void r_ensure_parent_dir(const char *path) {
    char *tmp = r_arena_strdup(path);
    char *slash = strrchr(tmp, '/');
    if (slash) {
        *slash = '\0';
        char *p = tmp; if (*p == '/') p++;
        while (*p) { if (*p == '/') { *p = '\0'; mkdir(tmp, 0755); *p = '/'; } p++; }
        mkdir(tmp, 0755);
    }
}

static RValue r_sys_fs_mkdir(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::mkdir requires string, got %s", r_value_type_name(path));
    r_ensure_parent_dir(path.as.string);
    return r_unit();
}

static RValue r_sys_fs_mkdir_parent(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::mkdir_parent requires a string path");
    r_ensure_parent_dir(path.as.string);
    return r_unit();
}

/* --- Process execution --- */
static RValue r_sys_process_run(RValue program, RValue args_val, int line) {
    if (program.type != R_TYPE_STRING)
        r_error(line, "sys::process_run requires string program, got %s", r_value_type_name(program));
    const char *prog_str = program.as.string;
    if (args_val.type != R_TYPE_ARRAY && args_val.type != R_TYPE_UNIT)
        r_error(line, "sys::process_run requires array, got %s", r_value_type_name(args_val));
    if (r_dry_run) {
        fprintf(stderr, "[dry-run] RUN %s\n", prog_str);
        return r_unit();
    }
    pid_t pid = fork();
    if (pid < 0) r_error(line, "fork failed: %s", strerror(errno));
    if (pid == 0) {
        char *argv[256]; int argc = 0; argv[argc++] = (char*)prog_str;
        if (args_val.type == R_TYPE_ARRAY)
            for (size_t i = 0; i < args_val.as.array->count; i++)
                if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                    argv[argc++] = args_val.as.array->elements[i].as.string;
        argv[argc] = NULL; execvp(argv[0], argv); _exit(127);
    }
    int status; waitpid(pid, &status, 0);
    if (WIFEXITED(status) && WEXITSTATUS(status) != 0)
        r_error(line, "process %s exited with status %d", prog_str, WEXITSTATUS(status));
    if (!WIFEXITED(status))
        r_error(line, "process %s terminated by signal", prog_str);
    return r_unit();
}

/* --- Parallel configuration --- */
static void r_sys_set_parallel_jobs(int jobs, int line) {
    if (jobs < 1) r_error(line, "parallel job count must be at least one");
    if (jobs > 1024) r_error(line, "parallel job count must not exceed 1024");
    r_parallel_jobs = jobs;
}

/* --- Generic parallel scheduler --- */
typedef struct {
    char *program;
    char *argv[64];
    int   argc;
} RGenericJob;

static RGenericJob *r_job_queue = NULL;
static int r_job_count = 0, r_job_capacity = 0;
static int r_parallel_collecting = 0;

static void r_parallel_begin(void) {
    r_job_count = 0;
    if (!r_job_queue) {
        r_job_capacity = 256;
        r_job_queue = (RGenericJob*)r_arena_alloc(r_job_capacity * sizeof(RGenericJob));
    }
}

/* Add a generic process job to the parallel queue. */
static void r_parallel_add_process(RValue program, RValue args_val, int line) {
    if (program.type != R_TYPE_STRING)
        r_error(line, "parallel requires string program");
    if (r_job_count >= r_job_capacity) {
        int cap = r_job_capacity * 2;
        RGenericJob *q = (RGenericJob*)r_arena_alloc(cap * sizeof(RGenericJob));
        memcpy(q, r_job_queue, r_job_count * sizeof(RGenericJob));
        r_job_queue = q; r_job_capacity = cap;
    }
    RGenericJob *job = &r_job_queue[r_job_count];
    job->program = r_arena_strdup(program.as.string);
    job->argc = 0;
    job->argv[job->argc++] = job->program;
    if (args_val.type == R_TYPE_ARRAY) {
        for (size_t i = 0; i < args_val.as.array->count && job->argc < 63; i++) {
            if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                job->argv[job->argc++] = args_val.as.array->elements[i].as.string;
        }
    }
    job->argv[job->argc] = NULL;
    r_job_count++;
}

typedef struct {
    int next, stop, total, failed_index, failed_status;
    pthread_mutex_t lock;
} RParallelState;

static void *r_parallel_worker(void *arg) {
    RParallelState *st = (RParallelState*)arg;
    while (1) {
        pthread_mutex_lock(&st->lock);
        int idx = st->next++;
        int stop = st->stop;
        pthread_mutex_unlock(&st->lock);
        if (idx >= st->total || stop) break;
        RGenericJob *job = &r_job_queue[idx];
        fprintf(stderr, "[%d/%d] %s\n", idx + 1, st->total, job->program);
        pid_t pid = fork();
        if (pid < 0) { pthread_mutex_lock(&st->lock); st->stop = 1; pthread_mutex_unlock(&st->lock); break; }
        if (pid == 0) { execvp(job->program, job->argv); _exit(127); }
        int status; waitpid(pid, &status, 0);
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            pthread_mutex_lock(&st->lock);
            st->stop = 1;
            st->failed_index = idx;
            st->failed_status = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
            pthread_mutex_unlock(&st->lock);
        }
    }
    return NULL;
}

static void r_parallel_execute(void) {
    if (r_job_count == 0) return;
    if (r_dry_run) {
        for (int i = 0; i < r_job_count; i++) {
            fprintf(stderr, "[dry-run] %s", r_job_queue[i].program);
            for (int j = 1; j < r_job_queue[i].argc; j++)
                fprintf(stderr, " %s", r_job_queue[i].argv[j]);
            fputc('\n', stderr);
        }
        r_job_count = 0;
        return;
    }
    int nw = r_parallel_jobs;
    if (nw > r_job_count) nw = r_job_count;
    if (nw < 1) nw = 1;
    if (nw <= 1) {
        for (int i = 0; i < r_job_count; i++) {
            RGenericJob *job = &r_job_queue[i];
            fprintf(stderr, "[%d/%d] %s\n", i + 1, r_job_count, job->program);
            pid_t pid = fork();
            if (pid == 0) { execvp(job->program, job->argv); _exit(127); }
            int status; waitpid(pid, &status, 0);
            if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
                r_error(0, "command failed: %s (exit %d)", job->program,
                        WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        }
    } else {
        RParallelState st = { 0, 0, r_job_count, -1, 0, PTHREAD_MUTEX_INITIALIZER };
        pthread_t *thr = (pthread_t*)r_arena_alloc(nw * sizeof(pthread_t));
        for (int i = 0; i < nw; i++) pthread_create(&thr[i], NULL, r_parallel_worker, &st);
        for (int i = 0; i < nw; i++) pthread_join(thr[i], NULL);
        pthread_mutex_destroy(&st.lock);
        if (st.stop) {
            RGenericJob *failed = &r_job_queue[st.failed_index];
            r_error(0, "command failed: %s (exit %d)", failed->program, st.failed_status);
        }
    }
    r_job_count = 0;
}

/* Legacy no-op for transpiler compatibility */
static void r_parallel_end(void) { /* no-op: results are handled per-job */ }

static void r_parallel_collect_begin(void) { r_parallel_collecting = 1; }
static void r_parallel_collect_end(void) { r_parallel_collecting = 0; }

/* --- Clean --- */
static int r_remove_directory(const char *path) {    DIR *dir = opendir(path);
    if (!dir) return 0;
    struct dirent *entry; char full[4096];
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) continue;
        snprintf(full, sizeof(full), "%s/%s", path, entry->d_name);
        struct stat st;
        if (stat(full, &st) == 0) {
            if (S_ISDIR(st.st_mode)) r_remove_directory(full);
            else unlink(full);
        }
    }
    closedir(dir); rmdir(path);
    return 1;
}

/* ========================================================================
 * sys::process — Process primitives
 * ======================================================================== */

/* Run process and capture stdout/stderr. Returns array [exit_code, stdout, stderr]. */
static RValue r_sys_process_output(RValue program, RValue args_val, int line) {
    if (program.type != R_TYPE_STRING)
        r_error(line, "sys::process::output requires string program, got %s", r_value_type_name(program));
    if (args_val.type != R_TYPE_ARRAY && args_val.type != R_TYPE_UNIT)
        r_error(line, "sys::process::output requires array args, got %s", r_value_type_name(args_val));

    if (r_parallel_collecting) {
        r_parallel_add_process(program, args_val, line);
        RValue scheduled = r_array_new();
        r_array_push(&scheduled, r_integer(0));
        r_array_push(&scheduled, r_string(""));
        r_array_push(&scheduled, r_string(""));
        return scheduled;
    }

    if (r_dry_run || r_verbose) {
        fprintf(stderr, "%s%s", r_dry_run ? "[dry-run] " : "+ ", program.as.string);
        if (args_val.type == R_TYPE_ARRAY)
            for (size_t i = 0; i < args_val.as.array->count; i++)
                if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                    fprintf(stderr, " %s", args_val.as.array->elements[i].as.string);
        fputc('\n', stderr);
    }
    if (r_dry_run) {
        RValue planned = r_array_new();
        r_array_push(&planned, r_integer(0));
        r_array_push(&planned, r_string(""));
        r_array_push(&planned, r_string(""));
        return planned;
    }

    int stdout_pipe[2], stderr_pipe[2];
    if (pipe(stdout_pipe) != 0 || pipe(stderr_pipe) != 0)
        r_error(line, "pipe failed: %s", strerror(errno));

    pid_t pid = fork();
    if (pid < 0) r_error(line, "fork failed: %s", strerror(errno));

    if (pid == 0) {
        /* Child: redirect stdout/stderr to pipes */
        close(stdout_pipe[0]); close(stderr_pipe[0]);
        dup2(stdout_pipe[1], STDOUT_FILENO);
        dup2(stderr_pipe[1], STDERR_FILENO);
        close(stdout_pipe[1]); close(stderr_pipe[1]);

        char *argv[256]; int argc = 0;
        argv[argc++] = (char*)program.as.string;
        if (args_val.type == R_TYPE_ARRAY)
            for (size_t i = 0; i < args_val.as.array->count; i++)
                if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                    argv[argc++] = args_val.as.array->elements[i].as.string;
        argv[argc] = NULL;
        execvp(argv[0], argv);
        _exit(127);
    }

    /* Parent: read output */
    close(stdout_pipe[1]); close(stderr_pipe[1]);

    /* Read stdout */
    char stdout_buf[65536]; size_t stdout_len = 0;
    ssize_t n;
    while ((n = read(stdout_pipe[0], stdout_buf + stdout_len, sizeof(stdout_buf) - stdout_len - 1)) > 0)
        stdout_len += n;
    stdout_buf[stdout_len] = '\0';
    close(stdout_pipe[0]);

    /* Read stderr */
    char stderr_buf[65536]; size_t stderr_len = 0;
    while ((n = read(stderr_pipe[0], stderr_buf + stderr_len, sizeof(stderr_buf) - stderr_len - 1)) > 0)
        stderr_len += n;
    stderr_buf[stderr_len] = '\0';
    close(stderr_pipe[0]);

    int status;
    waitpid(pid, &status, 0);
    int exit_code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;

    /* Return [exit_code, stdout, stderr] */
    RValue result = r_array_new();
    r_array_push(&result, r_integer((uint64_t)exit_code));
    r_array_push(&result, r_string(stdout_buf));
    r_array_push(&result, r_string(stderr_buf));
    return result;
}

/* Run process and return only the exit code (no capture). */
static RValue r_sys_process_status(RValue program, RValue args_val, int line) {
    if (program.type != R_TYPE_STRING)
        r_error(line, "sys::process::status requires string program");
    if (args_val.type != R_TYPE_ARRAY && args_val.type != R_TYPE_UNIT)
        r_error(line, "sys::process::status requires array args");

    pid_t pid = fork();
    if (pid < 0) r_error(line, "fork failed: %s", strerror(errno));
    if (pid == 0) {
        char *argv[256]; int argc = 0;
        argv[argc++] = (char*)program.as.string;
        if (args_val.type == R_TYPE_ARRAY)
            for (size_t i = 0; i < args_val.as.array->count; i++)
                if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                    argv[argc++] = args_val.as.array->elements[i].as.string;
        argv[argc] = NULL;
        execvp(argv[0], argv); _exit(127);
    }
    int status; waitpid(pid, &status, 0);
    int exit_code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    return r_integer((uint64_t)exit_code);
}

/* Spawn a background process. Returns PID. */
static RValue r_sys_process_spawn(RValue program, RValue args_val, int line) {
    if (program.type != R_TYPE_STRING)
        r_error(line, "sys::process::spawn requires string program");
    if (args_val.type != R_TYPE_ARRAY && args_val.type != R_TYPE_UNIT)
        r_error(line, "sys::process::spawn requires array args");

    pid_t pid = fork();
    if (pid < 0) r_error(line, "fork failed: %s", strerror(errno));
    if (pid == 0) {
        char *argv[256]; int argc = 0;
        argv[argc++] = (char*)program.as.string;
        if (args_val.type == R_TYPE_ARRAY)
            for (size_t i = 0; i < args_val.as.array->count; i++)
                if (args_val.as.array->elements[i].type == R_TYPE_STRING)
                    argv[argc++] = args_val.as.array->elements[i].as.string;
        argv[argc] = NULL;
        execvp(argv[0], argv); _exit(127);
    }
    return r_integer((uint64_t)pid);
}

/* Wait for a spawned process. Returns exit code. */
static RValue r_sys_process_wait(RValue pid_val, int line) {
    if (pid_val.type != R_TYPE_INTEGER)
        r_error(line, "sys::process::wait requires int pid");
    int status;
    pid_t result = waitpid((pid_t)pid_val.as.integer, &status, 0);
    if (result < 0) r_error(line, "waitpid failed: %s", strerror(errno));
    return r_integer((uint64_t)(WIFEXITED(status) ? WEXITSTATUS(status) : -1));
}

/* Send signal to process. */
static RValue r_sys_process_kill(RValue pid_val, RValue sig_val, int line) {
    if (pid_val.type != R_TYPE_INTEGER || sig_val.type != R_TYPE_INTEGER)
        r_error(line, "sys::process::kill requires int pid, int signal");
    int rc = kill((pid_t)pid_val.as.integer, (int)sig_val.as.integer);
    return r_integer((uint64_t)(rc == 0 ? 0 : -1));
}

/* ========================================================================
 * sys::fs — File I/O primitives
 * ======================================================================== */

/* Read entire file to string. */
static RValue r_sys_fs_read(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::read requires string path, got %s", r_value_type_name(path));
    FILE *f = fopen(path.as.string, "rb");
    if (!f) r_error(line, "cannot open %s: %s", path.as.string, strerror(errno));
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (size < 0) { fclose(f); r_error(line, "ftell failed for %s", path.as.string); }
    char *buf = (char*)r_arena_alloc((size_t)size + 1);
    size_t read = fread(buf, 1, (size_t)size, f);
    fclose(f);
    buf[read] = '\0';
    return r_string(buf);
}

/* Write string to file (create or overwrite). */
static RValue r_sys_fs_write(RValue path, RValue contents, int line) {
    if (path.type != R_TYPE_STRING || contents.type != R_TYPE_STRING)
        r_error(line, "sys::fs::write requires string path, string contents");
    r_ensure_parent_dir(path.as.string);
    FILE *f = fopen(path.as.string, "wb");
    if (!f) r_error(line, "cannot open %s for writing: %s", path.as.string, strerror(errno));
    size_t len = strlen(contents.as.string);
    size_t written = fwrite(contents.as.string, 1, len, f);
    fclose(f);
    if (written != len) r_error(line, "short write on %s", path.as.string);
    return r_unit();
}

/* Check if path exists. */
static RValue r_sys_fs_exists(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::exists requires string path");
    struct stat st;
    return r_boolean(stat(path.as.string, &st) == 0);
}

/* Check if path is a file. */
static RValue r_sys_fs_is_file(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::is_file requires string path");
    struct stat st;
    return r_boolean(stat(path.as.string, &st) == 0 && S_ISREG(st.st_mode));
}

/* Check if path is a directory. */
static RValue r_sys_fs_is_dir(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::is_dir requires string path");
    struct stat st;
    return r_boolean(stat(path.as.string, &st) == 0 && S_ISDIR(st.st_mode));
}

/* Get file size in bytes. */
static RValue r_sys_fs_size(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::size requires string path");
    struct stat st;
    if (stat(path.as.string, &st) != 0)
        r_error(line, "stat failed for %s: %s", path.as.string, strerror(errno));
    return r_integer((uint64_t)st.st_size);
}

/* Get file modification time (unix timestamp). */
static RValue r_sys_fs_mtime(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::mtime requires string path");
    struct stat st;
    if (stat(path.as.string, &st) != 0)
        r_error(line, "stat failed for %s: %s", path.as.string, strerror(errno));
    return r_integer((uint64_t)st.st_mtime);
}

/* Create directory (including parents). */
static RValue r_sys_fs_mkdir_all(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::mkdir_all requires string path");
    r_ensure_parent_dir(path.as.string);
    /* Also create the final directory */
    mkdir(path.as.string, 0755);
    return r_unit();
}

/* Remove a file. */
static RValue r_sys_fs_remove_file(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::remove_file requires string path");
    if (unlink(path.as.string) != 0 && errno != ENOENT)
        r_error(line, "cannot remove %s: %s", path.as.string, strerror(errno));
    return r_unit();
}

/* Remove directory recursively. */
static RValue r_sys_fs_remove_dir_all(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::remove_dir_all requires string path");
    r_remove_directory(path.as.string);
    return r_unit();
}

/* Rename/move file or directory. */
static RValue r_sys_fs_rename(RValue from, RValue to, int line) {
    if (from.type != R_TYPE_STRING || to.type != R_TYPE_STRING)
        r_error(line, "sys::fs::rename requires string from, string to");
    if (rename(from.as.string, to.as.string) != 0)
        r_error(line, "rename %s to %s failed: %s", from.as.string, to.as.string, strerror(errno));
    return r_unit();
}

/* Copy file. */
static RValue r_sys_fs_copy(RValue from, RValue to, int line) {
    if (from.type != R_TYPE_STRING || to.type != R_TYPE_STRING)
        r_error(line, "sys::fs::copy requires string from, string to");
    FILE *src = fopen(from.as.string, "rb");
    if (!src) r_error(line, "cannot open %s: %s", from.as.string, strerror(errno));
    r_ensure_parent_dir(to.as.string);
    FILE *dst = fopen(to.as.string, "wb");
    if (!dst) { fclose(src); r_error(line, "cannot open %s: %s", to.as.string, strerror(errno)); }
    char buf[8192]; size_t n;
    while ((n = fread(buf, 1, sizeof(buf), src)) > 0) fwrite(buf, 1, n, dst);
    fclose(src); fclose(dst);
    return r_unit();
}

/* List directory contents. Returns array of strings (names only). */
static RValue r_sys_fs_read_dir(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::fs::read_dir requires string path");
    DIR *dir = opendir(path.as.string);
    if (!dir) r_error(line, "cannot open directory %s: %s", path.as.string, strerror(errno));
    RValue result = r_array_new();
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) continue;
        r_array_push(&result, r_string(entry->d_name));
    }
    closedir(dir);
    return result;
}

/* ========================================================================
 * sys::io — Standard I/O and environment
 * ======================================================================== */

/* Read one line from stdin. Returns string (without trailing newline). */
static RValue r_sys_io_read_line(int line) {
    char buf[4096];
    if (!fgets(buf, sizeof(buf), stdin))
        return r_string("");
    size_t len = strlen(buf);
    if (len > 0 && buf[len - 1] == '\n') buf[len - 1] = '\0';
    return r_string(buf);
}

/* Print to stdout (no newline). */
static RValue r_sys_io_print(RValue text, int line) {
    if (text.type != R_TYPE_STRING)
        r_error(line, "sys::io::print requires string");
    printf("%s", text.as.string);
    return r_unit();
}

/* Print to stderr (no newline). */
static RValue r_sys_io_eprint(RValue text, int line) {
    if (text.type != R_TYPE_STRING)
        r_error(line, "sys::io::eprint requires string");
    fprintf(stderr, "%s", text.as.string);
    return r_unit();
}

/* Flush stdout. */
static RValue r_sys_io_flush(int line) {
    (void)line;
    fflush(stdout);
    return r_unit();
}

/* Get environment variable. Returns empty string if not set. */
static RValue r_sys_env_get(RValue name, int line) {
    if (name.type != R_TYPE_STRING)
        r_error(line, "sys::env::get requires string name");
    const char *val = getenv(name.as.string);
    return r_string(val ? val : "");
}

/* Set environment variable. */
static RValue r_sys_env_set(RValue name, RValue value, int line) {
    if (name.type != R_TYPE_STRING || value.type != R_TYPE_STRING)
        r_error(line, "sys::env::set requires string name, string value");
    setenv(name.as.string, value.as.string, 1);
    return r_unit();
}

/* Get current working directory. */
static RValue r_sys_env_cwd(int line) {
    char buf[4096];
    if (!getcwd(buf, sizeof(buf)))
        r_error(line, "getcwd failed: %s", strerror(errno));
    return r_string(buf);
}

/* Change working directory. */
static RValue r_sys_env_chdir(RValue path, int line) {
    if (path.type != R_TYPE_STRING)
        r_error(line, "sys::env::chdir requires string path");
    if (chdir(path.as.string) != 0)
        r_error(line, "chdir to %s failed: %s", path.as.string, strerror(errno));
    return r_unit();
}

/* Get command-line arguments. Requires main() to store them. */
static int r_main_argc = 0;
static char **r_main_argv = NULL;

static RValue r_sys_env_args(int line) {
    (void)line;
    RValue result = r_array_new();
    for (int i = 0; i < r_main_argc; i++)
        r_array_push(&result, r_string(r_main_argv[i]));
    return result;
}

/* Sleep for specified seconds. */
static RValue r_sys_time_sleep(RValue seconds, int line) {
    if (seconds.type != R_TYPE_INTEGER)
        r_error(line, "sys::time::sleep requires int seconds");
    sleep((unsigned int)seconds.as.integer);
    return r_unit();
}

/* Get monotonic timestamp in milliseconds. */
static RValue r_sys_time_now_ms(int line) {
    (void)line;
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return r_integer((uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000);
}

#endif /* ROLLER_RUNTIME_H */
