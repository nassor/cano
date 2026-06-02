document.addEventListener('DOMContentLoaded', () => {
  // ==========================================================================
  // Persistent DOM references — the sidebar / overlay / toggle live OUTSIDE the
  // Swup container, so they are never re-parsed. Anything bound to them is a
  // one-time setup; per-page work goes in initContent() / setActiveLinks().
  // ==========================================================================
  const menuToggle = document.getElementById('menu-toggle');
  const sidebar = document.querySelector('.sidebar');
  const overlay = document.querySelector('.sidebar-overlay');

  // --------------------------------------------------------------------------
  // Mobile menu toggle (one-time)
  // --------------------------------------------------------------------------
  function openSidebar() {
    if (!sidebar) return;
    sidebar.classList.add('open');
    if (overlay) overlay.classList.add('visible');
    if (menuToggle) menuToggle.setAttribute('aria-expanded', 'true');
    if (menuToggle) menuToggle.innerHTML = '&#10005;';
  }

  function closeSidebar() {
    if (!sidebar) return;
    sidebar.classList.remove('open');
    if (overlay) overlay.classList.remove('visible');
    if (menuToggle) menuToggle.setAttribute('aria-expanded', 'false');
    if (menuToggle) menuToggle.innerHTML = '&#9776;';
  }

  if (menuToggle) {
    menuToggle.addEventListener('click', () => {
      if (sidebar && sidebar.classList.contains('open')) {
        closeSidebar();
      } else {
        openSidebar();
      }
    });
  }

  // Close sidebar when clicking overlay
  if (overlay) {
    overlay.addEventListener('click', closeSidebar);
  }

  // Close sidebar when clicking outside on mobile (fallback if no overlay)
  document.addEventListener('click', (e) => {
    if (window.innerWidth <= 768 && sidebar && sidebar.classList.contains('open')) {
      if (!sidebar.contains(e.target) && menuToggle && !menuToggle.contains(e.target)) {
        closeSidebar();
      }
    }
  });

  // Close sidebar on Escape key
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && sidebar && sidebar.classList.contains('open')) {
      closeSidebar();
      if (menuToggle) menuToggle.focus();
    }
  });

  // --------------------------------------------------------------------------
  // Collapsible nav sections — persist the user's open/closed choice (one-time).
  // The INITIAL open/closed apply happens pre-paint in base.html's inline script;
  // here we only register the writer so toggling keeps the preference durable.
  // --------------------------------------------------------------------------
  const sectionStateKey = (section) =>
    'nav-section:' + (section.id || (section.querySelector('summary') || {}).textContent || '').trim();
  const readSectionState = (section) => {
    try { return localStorage.getItem(sectionStateKey(section)); } catch (e) { return null; }
  };
  document.querySelectorAll('details.nav-section').forEach((section) => {
    section.addEventListener('toggle', () => {
      try { localStorage.setItem(sectionStateKey(section), section.open ? 'open' : 'closed'); }
      catch (e) { /* storage unavailable — just fall back to HTML defaults */ }
    });
  });

  // --------------------------------------------------------------------------
  // Mermaid initialization (one-time). startOnLoad is false because we drive
  // rendering manually in initContent() so diagrams also render after a swap.
  // --------------------------------------------------------------------------
  if (window.mermaid) {
    mermaid.initialize({
      startOnLoad: false,
      theme: 'dark',
      securityLevel: 'loose',
      fontFamily: 'Outfit, sans-serif'
    });
  }

  // --------------------------------------------------------------------------
  // setActiveLinks() — runs on first load and after every content swap.
  // Highlights the current page's link and reveals its ancestor <details>
  // (unless the user explicitly collapsed it).
  // --------------------------------------------------------------------------
  const normalize = (p) => p.replace(/\/+$/, '') || '/';
  function setActiveLinks() {
    const currentPath = normalize(window.location.pathname);
    document.querySelectorAll('.nav-links a').forEach((link) => {
      const linkPath = normalize(new URL(link.href, window.location.origin).pathname);
      if (linkPath === currentPath) {
        link.classList.add('active');
        link.setAttribute('aria-current', 'page');
        let el = link.parentElement;
        while (el) {
          if (el.tagName === 'DETAILS' && readSectionState(el) !== 'closed') el.open = true;
          el = el.parentElement;
        }
      } else {
        link.classList.remove('active');
        link.removeAttribute('aria-current');
      }
    });
  }

  // --------------------------------------------------------------------------
  // initContent(scope) — idempotent per-page setup for the swapped-in content.
  // Runs on first load (scope = document) and after every Swup swap
  // (scope = #swup), so Prism / Mermaid / external links re-apply to new content.
  // --------------------------------------------------------------------------
  function initContent(scope) {
    scope = scope || document.getElementById('swup') || document;

    // Prism: Zola emits <code data-lang="X">; Prism expects class="language-X".
    // Bridge the two on the fresh content, then trigger highlight.
    scope.querySelectorAll('pre > code[data-lang]').forEach((el) => {
      el.classList.add('language-' + el.getAttribute('data-lang'));
    });
    if (window.Prism) {
      Prism.highlightAll();
    }

    // Mermaid: render only diagrams that haven't been processed yet, so a swap
    // never double-renders an existing diagram or skips a new one.
    if (window.mermaid) {
      const nodes = Array.from(scope.querySelectorAll('.mermaid:not([data-processed])'));
      if (nodes.length) {
        try { mermaid.run({ nodes }); } catch (e) { /* malformed diagram — leave as-is */ }
      }
    }

    // External links — open in new tab.
    scope.querySelectorAll('a[href^="http"]').forEach((link) => {
      if (link.hostname === window.location.hostname) return;
      if (!link.hasAttribute('target')) {
        link.setAttribute('target', '_blank');
        link.setAttribute('rel', 'noopener noreferrer');
      }
    });

    // Entrance animations (staggered fade-in for hero/feature elements).
    const animateElements = scope.querySelectorAll('.animate-in');
    if (animateElements.length > 0 && 'IntersectionObserver' in window) {
      const observer = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            if (entry.isIntersecting) {
              entry.target.style.animationPlayState = 'running';
              observer.unobserve(entry.target);
            }
          });
        },
        { threshold: 0.1, rootMargin: '0px 0px -40px 0px' }
      );

      animateElements.forEach((el) => {
        // Only observe elements that aren't already visible above the fold.
        const rect = el.getBoundingClientRect();
        if (rect.top > window.innerHeight) {
          el.style.animationPlayState = 'paused';
          observer.observe(el);
        }
      });
    }
  }

  // --------------------------------------------------------------------------
  // First load
  // --------------------------------------------------------------------------
  setActiveLinks();
  initContent(document);

  // --------------------------------------------------------------------------
  // Client-side navigation (Swup) — swaps only #swup (main content), leaving the
  // sidebar element untouched so its scroll position and open <details> persist
  // and there is no full-document repaint. Degrades to normal full-page
  // navigation if Swup failed to load.
  // --------------------------------------------------------------------------
  if (window.Swup) {
    const swup = new Swup({
      containers: ['#swup'],     // only the main content; sidebar is outside, never touched
      animationSelector: false   // instant swap — no fade, no new flash
    });

    // Close the mobile drawer when navigating so it doesn't cover fresh content.
    swup.hooks.on('visit:start', () => {
      if (window.innerWidth <= 768) closeSidebar();
    });

    // Re-run per-page setup on the swapped-in content.
    swup.hooks.on('content:replace', () => {
      initContent(document.getElementById('swup'));
      setActiveLinks();
    });

    // Accessibility: move focus into the freshly swapped content.
    swup.hooks.on('visit:end', () => {
      const main = document.getElementById('swup');
      if (main) {
        main.setAttribute('tabindex', '-1');
        main.focus({ preventScroll: true });
      }
    });
  }
});
