extern int libjs_rust_js_main(int, const char *const *, const void *);
int main(int argc, char **argv) {
    return libjs_rust_js_main(argc, (const char *const *)argv, 0);
}
