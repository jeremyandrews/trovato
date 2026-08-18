-- Alternative text for a managed file.
--
-- Media had no alt field at all, so every template that rendered an uploaded
-- image reached for the nearest string and used the filename
-- (`alt="{{ file.filename }}"`). A filename is not alternative text: at best it
-- is noise a screen reader reads aloud, at worst it is "IMG_4821.jpg" standing
-- in for the content of the image (WCAG F30).
--
-- Nullable on purpose, and NULL is meaningfully different from the empty string:
--
--   NULL          nobody has said what this image shows yet.
--   ''            explicitly decorative — an empty alt is the correct alt for an
--                 image that carries no information, per WCAG H67.
--
-- Existing rows are NULL, which is honest: nothing recorded alt text before this
-- column existed, and backfilling filenames would encode the defect as data.
ALTER TABLE file_managed ADD COLUMN alt_text VARCHAR(1024);

COMMENT ON COLUMN file_managed.alt_text IS
    'Alternative text for the image. NULL = never set; empty string = explicitly decorative.';
