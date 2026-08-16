# Building Your First Site with Trovato

This guide walks through setting up a traditional website with pages, blog posts, and navigation using a fresh Trovato installation.

## 1. Run the Installer

Start the Trovato server and navigate to `http://localhost:3000`. You'll be redirected to `/install` if the site hasn't been set up yet.

The installer prompts you to:
- Create an admin account (username, email, password)
- Set a site name and optional slogan
- Configure the site email address

Complete the wizard to finish installation.

## 2. Create a Home Page

1. Log in and go to **Admin > Content > Add Content > Page**
2. Enter a title like "Welcome to My Site"
3. Add body content describing your site
4. Set **Status** to Published and **Promote** to Yes
5. Save the page

To make this the dedicated front page:
- Note the item URL (e.g., `/item/01234567-...`)
- Set the `site_front_page` config key to that path. This can be done by inserting a row into the `site_config` table:
  ```sql
  INSERT INTO site_config (key, value)
  VALUES ('site_front_page', '"/item/YOUR-ITEM-UUID"')
  ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value;
  ```

If no front page is configured, the home page (`/`) automatically shows all promoted published content.

## 3. Create an About Page

1. Go to **Admin > Content > Add Content > Page**
2. Title: "About Us"
3. Add your about content
4. Set Status to Published
5. Save the page

Now add a URL alias so visitors can access it at `/about`:
- Edit the page and enter `/about` in the **URL Alias** field
- Save

Visit `http://localhost:3000/about` to see the page rendered with the site header, navigation, and footer.

## 4. Write Blog Posts

1. Go to **Admin > Content > Add Content > Blog**
2. Enter a title and body content
3. Set Status to Published
4. Save

Repeat to create several blog posts. Each post renders with the blog template showing the title, publication date, and body content.

## 5. Visit the Blog Listing

Navigate to `http://localhost:3000/blog` to see all published blog posts in reverse chronological order. The listing includes:

- Post titles linking to the full article
- Publication dates
- Body text previews
- Pagination (10 posts per page by default)

The `/blog` URL is automatically set up as an alias for the `blog_listing` Gather view.

## 6. Navigation

Navigation appears automatically in the site header. Out of the box you'll see:

- **Home** — links to `/`
- **Blog** — links to `/blog` (registered by the blog plugin)

Any plugin that registers public menu items (permission = empty) will automatically appear in the navigation bar, sorted by weight.

The navigation also shows contextual links:
- **Admin** and **Logout** for authenticated users
- **Login** for anonymous visitors

## Template Customization

Trovato uses a template suggestion system. To customize the appearance:

- `templates/page.html` — site-wide layout (header, nav, footer)
- `templates/elements/item--page.html` — page content type template
- `templates/elements/item--blog.html` — blog content type template
- `templates/page--front.html` — front page layout
- `templates/gather/view--blog_listing.html` — blog listing template

Templates are resolved from most specific to least specific. For example, viewing a blog post checks for:
1. `elements/item--blog--{uuid}.html` (specific item)
2. `elements/item--blog.html` (blog type)
3. `elements/item.html` (default)

Override any template by creating a more specific version in the `templates/` directory.
