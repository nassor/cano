/* ==========================================================================
   Cano Documentation: behaviour
   No framework, no bundler, no build step. Every block below is a small,
   independent enhancement: with JavaScript disabled the site is still
   readable and fully navigable through the sidebar and the inline
   "On this page" rails that the content authors by hand.
   ========================================================================== */

(() => {
  'use strict';

  // The theme is written pre-paint by the inline script in base.html; this key
  // has to match it exactly or the toggle and the first paint disagree.
  const THEME_KEY = 'cano:theme';

  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const scrollBehavior = reduceMotion ? 'auto' : 'smooth';

  /** Normalise a pathname for comparison: strip index.html and trailing slashes. */
  function normalisePath(pathname) {
    return pathname.replace(/index\.html$/, '').replace(/\/+$/, '') || '/';
  }

  /** Host comparison, not an `href^="http"` test: Zola's `get_url` emits
      absolute URLs, so a string test flags every internal link as external. */
  function isExternal(anchor) {
    return anchor.host !== window.location.host;
  }

  function escapeHtml(text) {
    return text.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  }

  /** Trailing-edge debounce: resize fires per frame while a window is dragged,
      and the work behind it is layout measurement. */
  function debounce(fn, wait) {
    let timer = 0;
    return () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(fn, wait);
    };
  }

  // --------------------------------------------------------------------------
  // Zola tags markdown code fences as `<code data-lang="rust">`, while Prism
  // only looks at `class="language-*"`. Zola normally emits both, but the
  // hand-written blocks in content pages carry whichever the author typed.
  //
  // This runs at deferred-script time, before Prism's own DOMContentLoaded
  // pass, so Prism sees the class on its first and only sweep.
  // --------------------------------------------------------------------------
  document.querySelectorAll('pre > code[data-lang]').forEach((code) => {
    const lang = code.dataset.lang;
    if (lang && !code.classList.contains(`language-${lang}`)) {
      code.classList.add(`language-${lang}`);
    }
  });

  document.addEventListener('DOMContentLoaded', () => {
    const root = document.documentElement;
    const header = document.querySelector('.site-header');
    const sidebar = document.getElementById('site-nav') || document.querySelector('.sidebar');
    const navToggle = document.getElementById('nav-toggle');
    const overlay = document.querySelector('.sidebar-overlay');
    const content = document.getElementById('content');

    // Measurements that no media query can express are re-run here on a single
    // debounced pass, rather than one listener per feature.
    const resizeTasks = [];

    // ------------------------------------------------------------------------
    // Theme
    //
    // The stylesheet resolves both palettes from one attribute on <html>, so
    // switching is a single write. The stored value is only ever set once the
    // reader picks a side; until then the operating system decides.
    // ------------------------------------------------------------------------
    const themeToggle = document.getElementById('theme-toggle');
    if (themeToggle) {
      const systemDark = window.matchMedia('(prefers-color-scheme: dark)');
      const current = () => root.dataset.theme || (systemDark.matches ? 'dark' : 'light');
      const label = () => {
        themeToggle.setAttribute('aria-label', current() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
      };

      label();
      themeToggle.addEventListener('click', () => {
        const next = current() === 'dark' ? 'light' : 'dark';
        root.dataset.theme = next;
        // Safari in private mode and locked-down enterprise profiles throw on
        // write; losing the preference is survivable, a broken toggle is not.
        try { localStorage.setItem(THEME_KEY, next); } catch (e) {}
        label();
      });

      // The label is wrong the moment the OS flips while no choice is stored.
      systemDark.addEventListener('change', label);
    }

    // ------------------------------------------------------------------------
    // Mobile navigation drawer
    // ------------------------------------------------------------------------
    function setDrawer(open) {
      if (!sidebar) return;
      sidebar.classList.toggle('open', open);
      if (overlay) overlay.classList.toggle('visible', open);
      if (navToggle) navToggle.setAttribute('aria-expanded', String(open));
      // Locking the body is what stops the page scrolling behind the drawer.
      document.body.classList.toggle('nav-open', open);
    }

    if (navToggle && sidebar) {
      navToggle.addEventListener('click', (event) => {
        event.stopPropagation();
        setDrawer(!sidebar.classList.contains('open'));
      });
    }

    if (overlay) overlay.addEventListener('click', () => setDrawer(false));

    // Following a link inside the drawer should close it: the target page is
    // rendered underneath and the reader would otherwise land behind the panel.
    if (sidebar) {
      sidebar.addEventListener('click', (event) => {
        if (event.target.closest('a')) setDrawer(false);
      });
    }

    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') setDrawer(false);
    });

    // At desktop width the sidebar is a permanent column, so a drawer left
    // open across a rotation would keep `body.nav-open` and freeze scrolling.
    const desktop = window.matchMedia('(min-width: 1024px)');
    desktop.addEventListener('change', (event) => {
      if (event.matches) setDrawer(false);
    });

    // ------------------------------------------------------------------------
    // Active nav link
    //
    // Compare normalised pathnames, not raw href strings: the hrefs are
    // absolute and the current URL may or may not carry a trailing slash.
    // ------------------------------------------------------------------------
    const here = normalisePath(window.location.pathname);
    let activeLink = null;

    document.querySelectorAll('.nav-links a').forEach((link) => {
      if (isExternal(link)) return;
      if (normalisePath(link.pathname) !== here) return;
      link.classList.add('active');
      link.setAttribute('aria-current', 'page');
      activeLink = link;
    });

    // Scroll the sidebar itself rather than calling scrollIntoView on the link:
    // that would also scroll the window and fight a #hash landing position.
    if (sidebar && activeLink) {
      const linkBox = activeLink.getBoundingClientRect();
      const navBox = sidebar.getBoundingClientRect();
      if (linkBox.top < navBox.top || linkBox.bottom > navBox.bottom) {
        sidebar.scrollTop += linkBox.top - navBox.top - (navBox.height - linkBox.height) / 2;
      }
    }

    // ------------------------------------------------------------------------
    // On this page
    //
    // Every content page authors its own `nav.page-toc` inline, so the entries
    // are curated (and correct with JavaScript off). Nothing is generated here:
    // the one node is *moved* between two homes. Wide viewports pin it in the
    // right rail beside the prose; narrower ones leave it in the flow, where it
    // reads as a card belonging to the page.
    // ------------------------------------------------------------------------
    const rail = document.getElementById('page-toc');
    const toc = content ? content.querySelector('nav.page-toc') : null;

    if (toc) {
      // The rail styling keys off `.toc-title`, while the authored markup says
      // `.page-toc-title`. Carrying both means one node styles correctly in
      // either home without the content having to know where it will end up.
      const tocTitle = toc.querySelector('.page-toc-title');
      if (tocTitle) tocTitle.classList.add('toc-title');

      // A pinned rail is a long way from the top of a long page.
      if (!toc.querySelector('.toc-top')) {
        const top = document.createElement('a');
        top.className = 'toc-top';
        top.href = '#';
        top.textContent = 'Back to top';
        top.addEventListener('click', (event) => {
          event.preventDefault();
          window.scrollTo({ top: 0, behavior: scrollBehavior });
        });
        toc.appendChild(top);
      }

      if (rail) {
        // A comment node holds the authored position exactly, so returning the
        // TOC to the prose cannot depend on remembering an index that later
        // DOM work (copy buttons, Prism) might have shifted.
        const anchor = document.createComment(' page-toc ');
        toc.parentNode.insertBefore(anchor, toc);

        const wide = window.matchMedia('(min-width: 1280px)');
        const placeToc = () => {
          if (wide.matches) {
            if (toc.parentNode !== rail) rail.appendChild(toc);
            rail.hidden = false;
            toc.classList.add('toc-inline');
            toc.classList.remove('toc-card');
          } else {
            // Guarded so a resize storm cannot detach and re-insert the node
            // on every event; re-parenting would reset any focus inside it.
            if (toc.previousSibling !== anchor) {
              anchor.parentNode.insertBefore(toc, anchor.nextSibling);
            }
            rail.hidden = true;
            toc.classList.add('toc-card');
            toc.classList.remove('toc-inline');
          }
        };

        placeToc();
        wide.addEventListener('change', placeToc);
        resizeTasks.push(placeToc);
      }

      // ----------------------------------------------------------------------
      // Scroll spy
      //
      // Driven by the links the author wrote, resolved to the headings they
      // point at. A stale entry (renamed heading) is skipped rather than
      // throwing, so one bad anchor cannot kill the whole rail.
      // ----------------------------------------------------------------------
      const spied = new Map();
      toc.querySelectorAll('a[href^="#"]').forEach((link) => {
        const id = decodeURIComponent(link.hash.slice(1));
        if (!id) return;
        const heading = document.getElementById(id);
        if (heading) spied.set(heading, link);
      });

      if (spied.size > 0 && 'IntersectionObserver' in window) {
        // The header is sticky, so the real top of the reading area sits below
        // it; the bottom margin keeps the highlight on the section being read
        // rather than the one just entering the viewport.
        const top = (header ? header.offsetHeight : 64) + 24;
        const spy = new IntersectionObserver(
          (entries) => {
            entries.forEach((entry) => {
              if (!entry.isIntersecting) return;
              const link = spied.get(entry.target);
              if (!link) return;
              spied.forEach((other) => other.classList.remove('reading'));
              link.classList.add('reading');
            });
          },
          { rootMargin: `-${top}px 0px -70% 0px` }
        );
        spied.forEach((_link, heading) => spy.observe(heading));
      }
    }

    // ------------------------------------------------------------------------
    // External links open in a new tab. Internal ones must not.
    // ------------------------------------------------------------------------
    document.querySelectorAll('.content a[href^="http"]').forEach((link) => {
      if (!isExternal(link)) return;
      link.setAttribute('rel', 'noopener noreferrer');
      if (!link.hasAttribute('target')) link.setAttribute('target', '_blank');
      // Image-only links (badges) get no ↗ mark: the glyph wraps below the
      // image and knocks it out of line with its neighbours.
      if (!link.querySelector('img')) link.classList.add('is-external');
    });

    // ------------------------------------------------------------------------
    // Copy buttons
    // ------------------------------------------------------------------------
    document.querySelectorAll('pre').forEach((pre) => {
      const code = pre.querySelector('code');
      if (!code || pre.querySelector('.copy-btn')) return;

      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'copy-btn';
      button.textContent = 'Copy';
      button.setAttribute('aria-label', 'Copy code to clipboard');

      const settle = (message, ok) => {
        button.textContent = message;
        button.classList.toggle('copied', ok);
        window.setTimeout(() => {
          button.textContent = 'Copy';
          button.classList.remove('copied');
        }, 1600);
      };

      // `navigator.clipboard` needs a secure context, which rules out plain
      // http previews and `file://`; the selection dance still works there.
      const legacyCopy = (text) => {
        const scratch = document.createElement('textarea');
        scratch.value = text;
        scratch.setAttribute('readonly', '');
        scratch.style.cssText = 'position:fixed;top:-1000px;opacity:0';
        document.body.appendChild(scratch);
        scratch.select();
        let ok = false;
        try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
        document.body.removeChild(scratch);
        return ok;
      };

      button.addEventListener('click', () => {
        const text = code.textContent;
        const fallback = () => {
          const ok = legacyCopy(text);
          settle(ok ? 'Copied' : 'Press Ctrl+C', ok);
        };
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).then(() => settle('Copied', true), fallback);
        } else {
          fallback();
        }
      });

      pre.appendChild(button);
    });

    // ------------------------------------------------------------------------
    // Back to top
    // ------------------------------------------------------------------------
    const backToTop = document.querySelector('.back-to-top');
    if (backToTop) {
      const onScroll = () => {
        backToTop.classList.toggle('visible', window.scrollY > 600);
      };
      onScroll();
      window.addEventListener('scroll', onScroll, { passive: true });
      backToTop.addEventListener('click', () => {
        window.scrollTo({ top: 0, behavior: scrollBehavior });
      });
    }

    // ------------------------------------------------------------------------
    // Reveal on scroll
    //
    // The class is added once and the element unobserved: these are entrances,
    // not a state that should flip back when the reader scrolls up.
    // ------------------------------------------------------------------------
    const revealTargets = document.querySelectorAll('.animate-in');
    if (revealTargets.length > 0) {
      if (reduceMotion || !('IntersectionObserver' in window)) {
        revealTargets.forEach((el) => el.classList.add('revealed'));
      } else {
        const reveal = new IntersectionObserver(
          (entries) => {
            entries.forEach((entry) => {
              if (!entry.isIntersecting) return;
              entry.target.classList.add('revealed');
              reveal.unobserve(entry.target);
            });
          },
          { threshold: 0.05, rootMargin: '0px 0px -32px 0px' }
        );
        revealTargets.forEach((el) => reveal.observe(el));
      }
    }

    // ------------------------------------------------------------------------
    // Diagram pan hint
    //
    // A diagram is authored at a fixed viewBox width and its labels are sized
    // for it, so the frame scrolls rather than scaling the type into
    // illegibility. Whether it actually scrolls depends on the column width,
    // which no media query knows precisely: sidebar width, rail and padding
    // all change at breakpoints. Measure, and only hint when it is true.
    // ------------------------------------------------------------------------
    const frames = Array.from(document.querySelectorAll('.diagram-frame'));
    if (frames.length > 0) {
      const syncPanHints = () => {
        frames.forEach((frame) => {
          const wrap = frame.querySelector('.cd-wrap');
          if (!wrap) return;
          // +1 absorbs sub-pixel rounding on fractional zoom levels.
          frame.classList.toggle('is-pannable', wrap.scrollWidth > wrap.clientWidth + 1);
        });
      };

      syncPanHints();
      if ('ResizeObserver' in window) {
        const ro = new ResizeObserver(syncPanHints);
        frames.forEach((frame) => {
          const wrap = frame.querySelector('.cd-wrap');
          if (wrap) ro.observe(wrap);
        });
      } else {
        resizeTasks.push(syncPanHints);
      }
    }

    // ------------------------------------------------------------------------
    // Search
    //
    // The index is one record per page, carrying its heading list, and is
    // fetched on first use rather than on page load: most readers never open
    // the palette. Everything here degrades to a visible empty state — a
    // missing or unreachable index must not throw on every keystroke.
    // ------------------------------------------------------------------------
    const dialog = document.getElementById('search-dialog');
    const searchInput = document.getElementById('search-input');
    const searchResults = document.getElementById('search-results');

    if (dialog && searchInput && searchResults && typeof dialog.showModal === 'function') {
      const searchOpen = document.getElementById('search-open');
      const closeBtn = dialog.querySelector('.cmdk-close');
      const indexUrl = dialog.dataset.index ? new URL(dialog.dataset.index, window.location.href) : null;
      const fileMode = window.location.protocol === 'file:';

      // Sentinels survive HTML escaping, so matches can be marked on the raw
      // text and turned into tags afterwards. Escaping first would let a term
      // like "amp" highlight the inside of an `&amp;` entity.
      const OPEN = '\u0001';
      const CLOSE = '\u0002';

      let records = null;
      let loading = null;
      let unavailable = !indexUrl || typeof fetch !== 'function';
      let items = [];
      let cursor = -1;

      function loadIndex() {
        if (records || unavailable) return Promise.resolve();
        if (!loading) {
          loading = fetch(indexUrl.href)
            .then((response) => {
              if (!response.ok) throw new Error(`index ${response.status}`);
              return response.json();
            })
            .then((data) => {
              records = Array.isArray(data) ? data : [];
              if (records.length === 0) unavailable = true;
            })
            .catch(() => { unavailable = true; });
        }
        return loading;
      }

      /** Every term must appear; a term at the start of a field ranks higher. */
      function scoreField(text, terms) {
        const lower = text.toLowerCase();
        let total = 0;
        for (const term of terms) {
          const at = lower.indexOf(term);
          if (at === -1) return 0;
          total += at === 0 ? 3 : 2;
        }
        return total;
      }

      /** Body hits are counted but capped, so a long page cannot outrank a
          precise title match through sheer repetition. */
      function scoreBody(text, terms) {
        const lower = text.toLowerCase();
        let total = 0;
        for (const term of terms) {
          const hits = lower.split(term).length - 1;
          if (hits === 0) return 0;
          total += Math.min(hits, 4);
        }
        return total;
      }

      function highlight(text, terms) {
        let marked = text;
        for (const term of terms) {
          const safe = term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
          marked = marked.replace(new RegExp(`(${safe})`, 'gi'), OPEN + '$1' + CLOSE);
        }
        return escapeHtml(marked).split(OPEN).join('<mark>').split(CLOSE).join('</mark>');
      }

      /** A window of body text around the earliest matched term. */
      function snippet(text, terms) {
        const lower = text.toLowerCase();
        let at = -1;
        for (const term of terms) {
          const found = lower.indexOf(term);
          if (found !== -1 && (at === -1 || found < at)) at = found;
        }
        const start = at === -1 ? 0 : Math.max(0, at - 60);
        let slice = text.slice(start, start + 180).trim();
        if (start > 0) slice = '…' + slice;
        if (start + 180 < text.length) slice += '…';
        return highlight(slice, terms);
      }

      /** Index URLs are site-relative ("guide/install/", "" for the home page)
          and resolve against the index file, which sits at the site root. An
          empty URL has to become that root, not the JSON file itself. */
      function resolve(url, id) {
        const target = new URL(url || './', indexUrl);
        // Opening a built site straight off disk: directory URLs need the file.
        if (fileMode && target.pathname.endsWith('/')) target.pathname += 'index.html';
        if (id) target.hash = id;
        return target.href;
      }

      function collect(terms) {
        const hits = [];

        for (const record of records) {
          const title = String(record.title || '');
          const crumb = String(record.crumb || '');
          const text = String(record.text || '');
          const group = crumb || title || 'Documentation';
          const titleScore = scoreField(title, terms);
          const bodyScore = scoreBody(text, terms);
          const crumbScore = scoreField(crumb, terms);

          if (titleScore || crumbScore || bodyScore) {
            hits.push({
              rank: titleScore * 8 + crumbScore * 4 + bodyScore,
              group,
              title,
              crumb: '',
              url: resolve(record.url, ''),
              text
            });
          }

          // A heading hit answers the query more precisely than the page that
          // contains it, so it outranks the page-level match and links at the
          // anchor. Its crumb names the page, which the title no longer does.
          const headings = Array.isArray(record.headings) ? record.headings : [];
          for (const heading of headings) {
            const label = String((heading && heading.text) || '');
            const id = String((heading && heading.id) || '');
            if (!label || !id) continue;
            const headingScore = scoreField(label, terms);
            if (!headingScore) continue;
            hits.push({
              rank: headingScore * 10 + titleScore * 2 + 1,
              group,
              title: label,
              crumb: title,
              url: resolve(record.url, id),
              text
            });
          }
        }

        return hits.sort((a, b) => b.rank - a.rank).slice(0, 12);
      }

      function empty(message) {
        const note = document.createElement('p');
        note.className = 'cmdk-empty';
        note.textContent = message;
        searchResults.appendChild(note);
      }

      function select(next) {
        if (items.length === 0) return;
        cursor = (next + items.length) % items.length;
        items.forEach((item, i) => item.setAttribute('aria-selected', String(i === cursor)));
        items[cursor].scrollIntoView({ block: 'nearest' });
      }

      function render(query) {
        const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
        searchResults.textContent = '';
        items = [];
        cursor = -1;
        searchInput.setAttribute('aria-expanded', String(terms.length > 0));

        if (unavailable) {
          empty('Search is unavailable — the index could not be loaded.');
          return;
        }
        if (terms.length === 0) {
          empty('Type to search every page, section by section.');
          return;
        }
        if (!records) {
          empty('Loading the index…');
          return;
        }

        const hits = collect(terms);
        if (hits.length === 0) {
          empty(`No matches for “${query}”.`);
          return;
        }

        let group = null;
        for (const hit of hits) {
          if (hit.group !== group) {
            group = hit.group;
            const label = document.createElement('div');
            label.className = 'cmdk-group-label';
            label.textContent = group;
            searchResults.appendChild(label);
          }

          const item = document.createElement('a');
          item.className = 'cmdk-item';
          item.href = hit.url;
          item.setAttribute('role', 'option');
          item.setAttribute('aria-selected', 'false');
          // Index text is untrusted markup: it is escaped first, so the only
          // real tags in here are the <mark> wrappers this file adds.
          item.innerHTML =
            `<span class="cmdk-item-title">${highlight(hit.title, terms)}</span>` +
            (hit.crumb ? `<span class="cmdk-item-crumb">${escapeHtml(hit.crumb)}</span>` : '') +
            `<span class="cmdk-item-text">${snippet(hit.text, terms)}</span>`;
          item.addEventListener('mouseenter', () => select(items.indexOf(item)));
          searchResults.appendChild(item);
          items.push(item);
        }

        select(0);
      }

      function openSearch() {
        if (dialog.open) return;
        dialog.showModal();
        render(searchInput.value);
        searchInput.focus();
        searchInput.select();
        loadIndex().then(() => { if (dialog.open) render(searchInput.value); });
      }

      // The trigger is rendered with the Windows and Linux spelling.
      const hint = searchOpen && searchOpen.querySelector('.kbd');
      if (hint && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)) {
        hint.textContent = '\u2318K';
      }

      if (searchOpen) searchOpen.addEventListener('click', openSearch);
      if (closeBtn) closeBtn.addEventListener('click', () => dialog.close());

      searchInput.addEventListener('input', () => render(searchInput.value));

      searchInput.addEventListener('keydown', (event) => {
        if (event.key === 'ArrowDown') {
          event.preventDefault();
          select(cursor + 1);
        } else if (event.key === 'ArrowUp') {
          event.preventDefault();
          select(cursor - 1);
        } else if (event.key === 'Enter' && cursor >= 0) {
          event.preventDefault();
          items[cursor].click();
        } else if (event.key === 'Escape') {
          // <dialog> cancels on Escape by itself, but only while the dialog
          // owns the key event; closing explicitly keeps it predictable.
          dialog.close();
        }
      });

      // Clicking the backdrop closes: the dialog element fills the viewport for
      // hit-testing, so compare the pointer against its own box.
      dialog.addEventListener('click', (event) => {
        const box = dialog.getBoundingClientRect();
        const inside = event.clientX >= box.left && event.clientX <= box.right &&
                       event.clientY >= box.top && event.clientY <= box.bottom;
        if (!inside) dialog.close();
      });

      document.addEventListener('keydown', (event) => {
        if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
          event.preventDefault();
          if (dialog.open) dialog.close(); else openSearch();
        }
      });
    }

    if (resizeTasks.length > 0) {
      window.addEventListener('resize', debounce(() => {
        resizeTasks.forEach((task) => task());
      }, 120));
    }

    // Prism autoloads on DOMContentLoaded unless the page opted out with
    // `data-manual`; highlighting twice is wasted work, so only drive it when
    // the page actually asked us to.
    if (window.Prism && window.Prism.manual) {
      window.Prism.highlightAll();
    }
  });
})();
