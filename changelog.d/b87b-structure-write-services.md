URL aliases, menu links, tiles and stages are written through services rather than by route handlers calling their models directly, so the next thing such a write has to do has one place to live. Two new services, `UrlAliasService` (`AppState::aliases`) and `MenuLinkService` (`AppState::menu_links`), plus write methods on the existing `TileService` and `StageService` (`update_stage`, `delete_stage`), cover every admin write of the four. The front-end item form's alias field goes through `UrlAliasService::set_for_item` and `clear_for_item`. The kernel defines no taps for any of these entities, so none are fired.

Deleting a menu link moved its children up to its parent in one statement and deleted the link in another, so a failure between the two left the children reparented under a link that was still there. Both now run in one transaction (`MenuLink::delete_in`).

Going passwordless (`POST /user/password/remove`) ran its own `UPDATE users`, so `tap_user_update` never fired for it. It now goes through `UserService::remove_password`, which also drops the cached user.

The stage visibility cache the item service once had is gone already: `ItemService::stage_view` reads stages from Postgres on every decision, so a stage write holds on the next request and there is nothing to clear.

The integration tests' `create_test_stage` named stages after the first eight digits of a v7 id, which are the clock, so two stages made in the same minute by a rerun collided on the machine name. It uses the random tail now.
