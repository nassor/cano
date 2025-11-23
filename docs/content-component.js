import { LitElement, html } from 'https://cdn.jsdelivr.net/npm/lit@3/+esm';
import { unsafeHTML } from 'https://cdn.jsdelivr.net/npm/lit@3/directives/unsafe-html.js/+esm';

export class ContentComponent extends LitElement {
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

customElements.define('content-component', ContentComponent);
