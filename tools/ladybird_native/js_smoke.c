extern int libjs_rust_js_main(int, const char *const *, const void *);
int main(void) {
    const char *arguments[] = {"/apps/program", "-c", "const values = [2, 3, 7]; console.log(values.reduce((n, v) => n * v, 1));", 0};
    return libjs_rust_js_main(3, arguments, 0);
}
