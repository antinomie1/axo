use std::path::PathBuf;

use axo_term::quote_paths;

#[test]
#[cfg(unix)]
fn dropped_paths_are_quoted_only_when_a_shell_would_split_or_expand_them() {
    let paths = [
        "/tmp/plain-file_1.txt",
        "/tmp/a b/ü.txt",
        "/tmp/it's",
        "/tmp/$HOME*",
    ]
    .map(PathBuf::from);
    assert_eq!(
        quote_paths(&paths),
        r"/tmp/plain-file_1.txt '/tmp/a b/ü.txt' '/tmp/it'\''s' '/tmp/$HOME*' "
    );
    assert_eq!(quote_paths(&[]), "");
}
