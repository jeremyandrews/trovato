A config set can ship files. The only way to create a `file_managed` row was
`POST /file/upload` with a session and a CSRF header, so a set could name a
file by id in an item's File field but never carry the file itself.

`file.<uuid>.yml` is a new config entity (`filename`, `filemime`, optional
`alt_text`, and `path`, the bytes' location relative to the config set
directory, by convention `files/<filename>`). Import reads and validates the
bytes in its validation pass, through `FileService::validate_file`, the same
size, allowlist, magic byte and extension checks an upload gets, so a bad file
fails the whole set before anything is written; a path that is absolute, uses
`..` or leads out of the directory through a symlink is refused the same way.
The file is stored `Permanent` under its declared id through
`FileService::store`, the one write path upload now uses too; re-import keeps
the id and storage URI and rewrites the bytes only when they changed. Files
import before items, so the item's reference is indexed on the same run.
Export writes every permanent file's YAML and bytes. Files are not staged.
