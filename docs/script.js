import { LitElement, html } from 'https://cdn.jsdelivr.net/npm/lit@3/+esm';
import { unsafeHTML } from 'https://cdn.jsdelivr.net/npm/lit@3/directives/unsafe-html.js/+esm';

// Menu Component
class MenuComponent extends LitElement {
  static properties = {
    currentPage: { type: String }
  };

  // Don't use shadow DOM so global styles apply
  createRenderRoot() {
    return this;
  }

  constructor() {
    super();
    this.currentPage = 'home';
  }

  connectedCallback() {
    super.connectedCallback();
    // Get initial page from URL hash or default to home
    this.currentPage = window.location.hash.slice(1) || 'home';
    
    // Listen for hash changes
    window.addEventListener('hashchange', () => {
      this.currentPage = window.location.hash.slice(1) || 'home';
    });
  }

  _navigate(page) {
    window.location.hash = page;
    this.currentPage = page;
    
    // Close sidebar on mobile
    if (window.innerWidth <= 768) {
      const sidebar = this.querySelector('.sidebar');
      if (sidebar) {
        sidebar.classList.remove('open');
        const menuToggle = document.getElementById('menu-toggle');
        if (menuToggle) {
          menuToggle.innerHTML = '☰';
        }
      }
    }
  }

  render() {
    return html`
      <nav class="sidebar">
        <a href="#home" class="logo" @click=${(e) => { e.preventDefault(); this._navigate('home'); }}>
          <img src="logo.png" alt="Cano" style="height: 24px; vertical-align: middle; margin-right: 8px;">
          Cano
        </a>
        <ul class="nav-links">
          <li><a href="#home" class="${this.currentPage === 'home' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('home'); }}>Home</a></li>
          <li><a href="#task" class="${this.currentPage === 'task' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('task'); }}>Tasks</a></li>
          <li><a href="#nodes" class="${this.currentPage === 'nodes' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('nodes'); }}>Nodes</a></li>
          <li><a href="#workflows" class="${this.currentPage === 'workflows' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('workflows'); }}>Workflows</a></li>
          <li><a href="#scheduler" class="${this.currentPage === 'scheduler' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('scheduler'); }}>Scheduler</a></li>
          <li><a href="#tracing" class="${this.currentPage === 'tracing' ? 'active' : ''}" 
                 @click=${(e) => { e.preventDefault(); this._navigate('tracing'); }}>Tracing</a></li>
        </ul>
      </nav>
    `;
  }
}

// Content Component
class ContentComponent extends LitElement {
  static properties = {
    page: { type: String },
    content: { type: String }
  };

  // Don't use shadow DOM so global styles apply
  createRenderRoot() {
    return this;
  }

  constructor() {
    super();
    this.page = 'home';
    this.content = '';
  }

  connectedCallback() {
    super.connectedCallback();
    
    // Get initial page from URL hash or default to home
    const initialPage = window.location.hash.slice(1) || 'home';
    this.loadContent(initialPage);
    
    // Listen for hash changes
    window.addEventListener('hashchange', () => {
      const page = window.location.hash.slice(1) || 'home';
      this.loadContent(page);
    });
  }

  async loadContent(page) {
    this.page = page;
    try {
      const response = await fetch(`content/${page}.html`);
      if (response.ok) {
        this.content = await response.text();
        // After content is loaded, re-initialize Prism and Mermaid
        this.requestUpdate();
        await this.updateComplete;
        this._reinitializeLibraries();
      } else {
        this.content = '<h1>Page not found</h1><p>Sorry, the requested page could not be loaded.</p>';
        this.requestUpdate();
      }
    } catch (error) {
      this.content = `<h1>Error</h1><p>Failed to load content: ${error.message}</p>`;
      this.requestUpdate();
    }
  }

  _reinitializeLibraries() {
    // Get the actual DOM content (no shadow DOM)
    const contentElement = this.querySelector('.content-wrapper');
    if (!contentElement) return;

    // Re-highlight code blocks with Prism
    if (window.Prism) {
      window.Prism.highlightAllUnder(contentElement);
    }

    // Re-render Mermaid diagrams
    if (window.mermaid) {
      window.mermaid.init(undefined, contentElement.querySelectorAll('.mermaid'));
    }
  }

  render() {
    return html`
      <main class="main-content">
        <div class="content-wrapper">
          ${unsafeHTML(this.content)}
        </div>
      </main>
    `;
  }
}

// Register custom elements
customElements.define('menu-component', MenuComponent);
customElements.define('content-component', ContentComponent);

// Initialize Mermaid and mobile menu toggle
document.addEventListener('DOMContentLoaded', () => {
    // Initialize Mermaid
    mermaid.initialize({
        startOnLoad: true,
        theme: 'dark',
        securityLevel: 'loose',
        fontFamily: 'Outfit, sans-serif'
    });

    // Mobile Menu Toggle
    const menuToggle = document.getElementById('menu-toggle');
    const menuComponent = document.querySelector('menu-component');

    if (menuToggle && menuComponent) {
        menuToggle.addEventListener('click', () => {
            const sidebar = menuComponent.querySelector('.sidebar');
            if (sidebar) {
                sidebar.classList.toggle('open');
                const icon = sidebar.classList.contains('open') ? '✕' : '☰';
                menuToggle.innerHTML = icon;
            }
        });
    }

    // Close sidebar when clicking outside on mobile
    document.addEventListener('click', (e) => {
        if (window.innerWidth <= 768 && menuComponent) {
            const sidebar = menuComponent.querySelector('.sidebar');
            if (sidebar && !sidebar.contains(e.target) && 
                !menuToggle.contains(e.target) && 
                sidebar.classList.contains('open')) {
                sidebar.classList.remove('open');
                menuToggle.innerHTML = '☰';
            }
        }
    });
});
