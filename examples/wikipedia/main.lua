-- Wikipedia example for pinhead
-- Demonstrates search, article browsing, lazy link loading, and bookmarking
-- using the Wikipedia API and filesystem operations.
--
-- Ported from nklhd's wikipedia example.
--
-- Usage:
--   ninep.listen("sock:/tmp/wikipedia.sock")
--   fuse.mount("/tmp/wikipedia")
--   echo "american folk music" > /tmp/wikipedia/search
--   ls /tmp/wikipedia/result/
--   cat /tmp/wikipedia/result/woody_guthrie.md

log.print("Loading Wikipedia example script")

-- Determine script directory for persistent bookmarks
local cwd = __pinhead_cwd or "."
local bookmarks_db = doc.open(cwd .. "/bookmarks.db")

-- Transport configuration — override via env vars for e2e tests
if env.get("PINHEAD_LISTEN") then
    ninep.listen(env.get("PINHEAD_LISTEN"))
elseif env.get("PINHEAD_SSH_LISTEN") then
    sshfs.listen(env.get("PINHEAD_SSH_LISTEN"))
elseif env.get("PINHEAD_FUSE_MOUNT") then
    fuse.mount(env.get("PINHEAD_FUSE_MOUNT"))
else
    -- ninep.listen("sock:/tmp/pinhead.sock")
    -- sshfs.listen("127.0.0.1:2222")
    -- fuse.mount("/tmp/pinhead")
end

-- Helper: list bookmark keys matching a virtual directory prefix.
-- Returns unique child names (files and "directories").
local function list_bookmarks(prefix)
    prefix = "bookmark:" .. prefix
    if prefix ~= "bookmark:" and prefix:sub(-1) ~= "/" then
        prefix = prefix .. "/"
    end
    local pattern = prefix .. "%"
    local rows = sql.query(bookmarks_db, "SELECT key FROM docs WHERE key LIKE ?1 ORDER BY key", {pattern})
    local seen = {}
    local results = {}
    for _, row in ipairs(rows) do
        local remainder = row.key:sub(#prefix + 1)
        if remainder and remainder ~= "" then
            local child = remainder:match("^([^/]+)")
            if child and not seen[child] then
                seen[child] = true
                -- If there's more after the first component, it's a directory
                local is_dir = remainder:find("/")
                if is_dir then
                    table.insert(results, child .. "/")
                else
                    table.insert(results, child)
                end
            end
        end
    end
    return table.concat(results, "\n")
end
local function url_encode(str)
    if str == nil then return "" end
    str = string.gsub(str, "([^%w%-%.%_%~ ])", function(c)
        return string.format("%%%02X", string.byte(c))
    end)
    str = string.gsub(str, " ", "+")
    return str
end

-- Helper: make a request to the Wikipedia API
local function wikipedia_api(params)
    local base_url = "https://en.wikipedia.org/w/api.php"
    local query_parts = {}
    for k, v in pairs(params) do
        table.insert(query_parts, k .. "=" .. url_encode(tostring(v)))
    end
    local url = base_url .. "?" .. table.concat(query_parts, "&") .. "&format=json"

    log.print("Fetching Wikipedia API: " .. url)
    local ok, result = pcall(req.get, url, {
        timeout_ms = 10000,
        headers = {
            ["User-Agent"] = "pinhead-wikipedia-example/1.0"
        }
    })
    if not ok then
        log.print("Wikipedia API error: " .. tostring(result))
        return nil, tostring(result)
    end

    local ok2, data = pcall(json.dec, result)
    if not ok2 then
        log.print("JSON decode error: " .. tostring(data))
        log.print("Response body (first 200 chars): " .. string.sub(result, 1, 200))
        return nil, "JSON decode error"
    end

    return data
end

-- Search Wikipedia articles
local function wikipedia_search(query)
    local data, err = wikipedia_api({
        action = "query",
        list = "search",
        srsearch = query,
        srlimit = "10",
        srprop = "snippet|titlesnippet",
        utf8 = "1"
    })
    if err ~= nil then return nil, err end

    local results = {}
    if data.query and data.query.search then
        for _, item in ipairs(data.query.search) do
            table.insert(results, {
                title = item.title,
                snippet = item.snippet,
                pageid = item.pageid
            })
        end
    end
    return results
end

-- Get article content (extract)
local function wikipedia_article(title)
    local data, err = wikipedia_api({
        action = "query",
        prop = "extracts|links",
        titles = title,
        explaintext = "1",
        pllimit = "20",
        utf8 = "1"
    })
    if err ~= nil then return nil, err end

    local pages = data.query and data.query.pages
    if not pages then return nil, "No pages found" end

    local page_key = next(pages)
    if not page_key then return nil, "No pages found" end
    local page = pages[page_key]
    if not page or page.missing then
        return nil, "Article not found"
    end

    local content = page.extract or ""
    local links = {}
    if page.links then
        for _, link in ipairs(page.links) do
            table.insert(links, link.title)
        end
    end

    return {
        title = title,
        content = content,
        links = links
    }
end

-- Convert article to markdown format
local function article_to_markdown(article)
    local md = "# " .. article.title .. "\n\n"
    md = md .. article.content .. "\n\n"
    md = md .. "## Links\n\n"
    for i, link in ipairs(article.links) do
        md = md .. string.format("%d. [%s](article/%s/links/%d)\n", i, link, article.title, i)
    end
    return md
end

-- State storage (in-memory for search results and cached articles)
local state = {
    search_results = {},
    articles = {},
    article_links = {},
}

-- Route: write to /search triggers a search
route.write("/search", function(params, data)
    local query = data:gsub("%s+$", ""):gsub("^%s+", "")
    if query == "" then
        return "Error: empty search query"
    end

    log.print("Searching Wikipedia for: " .. query)
    local results, err = wikipedia_search(query)
    if err ~= nil then
        return "Search error: " .. err
    end

    -- Store results in state
    state.search_results = {}
    for _, result in ipairs(results) do
        state.search_results[result.title] = result
    end

    return "Search completed. " .. #results .. " results found.\n" ..
           "Run: ls result/ to see results.\n" ..
           "Or: cat result/Article_Title.md to read an article."
end)

-- Directory listing for /result
route.readdir("/result", function(params, data)
    local files = {}
    for title, _ in pairs(state.search_results) do
        local filename = title:gsub("[ /]", "_") .. ".md"
        table.insert(files, filename)
    end
    return table.concat(files, "\n")
end)

-- Read a search result article (markdown)
route.read("/result/{title}.md", function(params, data)
    local title = params.title
    if not title then
        return "Error: could not determine article title"
    end
    title = title:gsub("_", " ")

    local result = state.search_results[title]
    if not result then
        return "Error: article not found in search results"
    end

    -- Check if article already cached
    if not state.articles[title] then
        local article, err = wikipedia_article(title)
        if err ~= nil then
            return "Error fetching article: " .. err
        end
        state.articles[title] = article
        state.article_links[title] = article.links
    end

    return article_to_markdown(state.articles[title])
end)

-- Directory listing for article links
route.readdir("/article/{title}/links", function(params, data)
    local title = params.title
    if not title then
        return {}
    end
    title = title:gsub("_", " ")

    local links = state.article_links[title]
    if not links then
        return {}
    end

    local files = {}
    for i = 1, #links do
        table.insert(files, tostring(i))
    end
    return table.concat(files, "\n")
end)

-- Read a specific link (lazy fetch)
route.read("/article/{title}/links/{link_id}", function(params, data)
    local title = params.title
    if not title then
        return "Error: could not determine article title"
    end
    title = title:gsub("_", " ")

    local link_id = tonumber(params.link_id)
    if not link_id then
        return "Error: invalid link ID"
    end

    local links = state.article_links[title]
    if not links or link_id < 1 or link_id > #links then
        return "Error: invalid link ID"
    end

    local link_title = links[link_id]
    -- Fetch the linked article (cache)
    if not state.articles[link_title] then
        local article, err = wikipedia_article(link_title)
        if err ~= nil then
            return "Error fetching linked article: " .. err
        end
        state.articles[link_title] = article
        state.article_links[link_title] = article.links
    end

    -- Return a summary with link to read full article
    local article = state.articles[link_title]
    local summary = article.content:sub(1, 500) .. "..."
    return string.format("# %s\n\n%s\n\n[Read full article](result/%s.md)",
        link_title, summary, link_title:gsub("[ /]", "_"))
end)

-- Bookmark system (persistent via doc store)

-- Write to a bookmark file
route.write("/bookmarks/{path}", function(params, data)
    local virtual_path = params.path
    if not virtual_path then
        return "Error: could not determine bookmark path"
    end
    doc.set(bookmarks_db, "bookmark:" .. virtual_path, {content = data})
    return "Bookmark saved: " .. virtual_path
end)

-- Read a bookmark file
route.read("/bookmarks/{path}", function(params, data)
    local virtual_path = params.path
    if not virtual_path then
        return "Error: could not determine bookmark path"
    end
    local val = doc.get(bookmarks_db, "bookmark:" .. virtual_path)
    if not val then
        return "Error: bookmark not found"
    end
    return val.content or ""
end)

-- Create a bookmark directory (mkdir command)
route.create("/bookmarks/{path}", function(params, data)
    local virtual_path = params.path
    if not virtual_path then
        return "Error: could not determine bookmark path"
    end
    -- Directories are implicit in the doc store — just confirm it doesn't
    -- conflict with an existing bookmark file.
    local existing = doc.get(bookmarks_db, "bookmark:" .. virtual_path)
    if existing then
        return "Error: a bookmark with that name already exists"
    end
    -- Check if any bookmarks exist under this prefix (directory already "exists")
    local rows = sql.query(bookmarks_db, "SELECT key FROM docs WHERE key LIKE ?1 LIMIT 1",
        {"bookmark:" .. virtual_path .. "/%"})
    if #rows > 0 then
        return "Error: already exists"
    end
    return "Directory created: " .. virtual_path
end)

-- Remove a bookmark or directory
route.unlink("/bookmarks/{path}", function(params, data)
    local virtual_path = params.path
    if not virtual_path then
        return "Error: could not determine bookmark path"
    end
    -- Check if it's a "directory" (has children)
    local children = sql.query(bookmarks_db, "SELECT key FROM docs WHERE key LIKE ?1",
        {"bookmark:" .. virtual_path .. "/%"})
    if #children > 0 then
        -- Remove all children (recursive delete)
        for _, row in ipairs(children) do
            doc.delete(bookmarks_db, row.key)
        end
        return "Removed directory: " .. virtual_path
    end
    -- Remove a single bookmark
    local existed = doc.get(bookmarks_db, "bookmark:" .. virtual_path)
    if not existed then
        return "Error: not found"
    end
    doc.delete(bookmarks_db, "bookmark:" .. virtual_path)
    return "Removed: " .. virtual_path
end)

-- List bookmarks directory
route.readdir("/bookmarks/{path}", function(params, data)
    local virtual_path = params.path or ""
    return list_bookmarks(virtual_path)
end)

-- Root directory listing
route.readdir("/", function(params, data)
    return "search\nresult/\narticle/\nbookmarks/\nREADME.md"
end)

-- README file
route.read("/README.md", function(params, data)
    return [[# Wikipedia Example

This example demonstrates a Wikipedia client using pinhead's virtual filesystem.

## Usage

1. Search for articles:
   ```bash
   echo "american folk music" > search
   ```

2. List search results:
   ```bash
   ls result/
   ```

3. Read an article:
   ```bash
   cat result/woody_guthrie.md
   ```

4. Explore article links (lazy-loaded):
   ```bash
   ls article/woody_guthrie/links/
   cat article/woody_guthrie/links/1
   ```

5. Bookmark articles:
   ```bash
   cp result/woody_guthrie.md bookmarks/music/
   ```

6. Create bookmark categories:
   ```bash
   mkdir bookmarks/music
   ```

## Features

- Real Wikipedia API integration
- Lazy loading of links
- Hierarchical bookmark system with persistence (stored in a `bookmarks/` directory)
- Markdown formatting

## Implementation Notes

- Uses pinhead's `req.get()` for API requests
- Uses pinhead's `fs` module for persistent bookmark storage
- Caches articles and links in memory
- Demonstrates read, write, readdir, create, unlink operations
]]
end)

log.print("Wikipedia example loaded successfully")
