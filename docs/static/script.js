document.addEventListener('DOMContentLoaded', () => {
  // --------------------------------------------------------------------------
  // Prism.js: Zola emits <code data-lang="X">; Prism expects class="language-X".
  // Bridge the two, then trigger highlight.
  // --------------------------------------------------------------------------
  document.querySelectorAll('pre > code[data-lang]').forEach((el) => {
    el.classList.add('language-' + el.getAttribute('data-lang'));
  });
  if (window.Prism) {
    Prism.highlightAll();
  }

  // --------------------------------------------------------------------------
  // Mermaid initialization
  // --------------------------------------------------------------------------
  if (window.mermaid) {
    mermaid.initialize({
      startOnLoad: true,
      theme: 'dark',
      securityLevel: 'loose',
      fontFamily: 'Outfit, sans-serif'
    });
  }

  // --------------------------------------------------------------------------
  // DOM references
  // --------------------------------------------------------------------------
  const menuToggle = document.getElementById('menu-toggle');
  const sidebar = document.querySelector('.sidebar');
  const overlay = document.querySelector('.sidebar-overlay');

  // --------------------------------------------------------------------------
  // Mobile menu toggle
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
  // Collapsible nav sections — persist the user's open/closed choice
  // --------------------------------------------------------------------------
  const sectionStateKey = (section) =>
    'nav-section:' + (section.id || (section.querySelector('summary') || {}).textContent || '').trim();
  const readSectionState = (section) => {
    try { return localStorage.getItem(sectionStateKey(section)); } catch (e) { return null; }
  };
  document.querySelectorAll('details.nav-section').forEach((section) => {
    const stored = readSectionState(section);
    if (stored === 'open') section.open = true;
    else if (stored === 'closed') section.open = false;
    section.addEventListener('toggle', () => {
      try { localStorage.setItem(sectionStateKey(section), section.open ? 'open' : 'closed'); }
      catch (e) { /* storage unavailable — just fall back to HTML defaults */ }
    });
  });

  // --------------------------------------------------------------------------
  // Active nav link highlighting
  // --------------------------------------------------------------------------
  const normalize = (p) => p.replace(/\/+$/, '') || '/';
  const currentPath = normalize(window.location.pathname);

  const navLinks = document.querySelectorAll('.nav-links a');
  navLinks.forEach((link) => {
    const linkPath = normalize(new URL(link.href, window.location.origin).pathname);
    if (linkPath === currentPath) {
      link.classList.add('active');
      link.setAttribute('aria-current', 'page');
      // Reveal every <details> ancestor of the current page — section *and* any nested
      // sub-dropdown — unless the user explicitly collapsed it.
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

  // --------------------------------------------------------------------------
  // Entrance animations (staggered fade-in for hero/feature elements)
  // --------------------------------------------------------------------------
  const animateElements = document.querySelectorAll('.animate-in');
  if (animateElements.length > 0) {
    // Use IntersectionObserver for elements below the fold
    if ('IntersectionObserver' in window) {
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
        // Only observe elements that aren't already visible above fold
        const rect = el.getBoundingClientRect();
        if (rect.top > window.innerHeight) {
          el.style.animationPlayState = 'paused';
          observer.observe(el);
        }
      });
    }
  }

  // --------------------------------------------------------------------------
  // External link handling — open in new tab
  // --------------------------------------------------------------------------
  document.querySelectorAll('a[href^="http"]').forEach((link) => {
    if (link.hostname === window.location.hostname) return;
    if (!link.hasAttribute('target')) {
      link.setAttribute('target', '_blank');
      link.setAttribute('rel', 'noopener noreferrer');
    }
  });
});
