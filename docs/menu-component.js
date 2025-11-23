import { LitElement, html } from 'https://cdn.jsdelivr.net/npm/lit@3/+esm';

export class MenuComponent extends LitElement {
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

customElements.define('menu-component', MenuComponent);
