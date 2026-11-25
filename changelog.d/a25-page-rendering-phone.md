A search result for a page builder item no longer shows its page builder
document as its snippet. Every such result on `/search` read
`{"root": {"props": {}}, "content": [{"type": "TextBlock", …` under its title,
on every screen, and on a phone the unbroken JSON ran the results page wider
than the screen.

The snippet source (`search::snippet::SOURCE_SQL`) fell back from
`field_body->>'value'` to `fields->>'field_body'`, and `->>` on a JSON object
returns the object's JSON text. A page builder field is exactly that object,
with no `value` key. The bare field is now read as text only when it is a JSON
string, the same rule `item_search_vector` already indexed by, so the snippet
shows what the index holds: the title, and a long text body where there is one.
