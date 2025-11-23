document.addEventListener('DOMContentLoaded', () => {
  // Initialize Mermaid
  if (window.mermaid) {
    mermaid.initialize({
      startOnLoad: true,
      theme: 'dark',
      securityLevel: 'loose',
      fontFamily: 'Outfit, sans-serif'
    });
  }

  const menuToggle = document.getElementById('menu-toggle');
  const sidebar = document.querySelector('.sidebar');

  // Mobile Menu Toggle
  if (menuToggle && sidebar) {
    menuToggle.addEventListener('click', () => {
      sidebar.classList.toggle('open');
      const icon = sidebar.classList.contains('open') ? '✕' : '☰';
      menuToggle.innerHTML = icon;
    });
  }

  // Close sidebar when clicking outside on mobile
  document.addEventListener('click', (e) => {
    if (window.innerWidth <= 768 && sidebar && sidebar.classList.contains('open')) {
      if (!sidebar.contains(e.target) && !menuToggle.contains(e.target)) {
        sidebar.classList.remove('open');
        menuToggle.innerHTML = '☰';
      }
    }
  });
});
