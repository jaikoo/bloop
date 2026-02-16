// ── Sidebar Toggle (mobile) ──
const sidebar = document.querySelector('.sidebar');
const sidebarToggle = document.querySelector('.sidebar-toggle');

if (sidebarToggle) {
  sidebarToggle.addEventListener('click', () => {
    sidebar.classList.toggle('open');
    sidebarToggle.setAttribute('aria-expanded', sidebar.classList.contains('open'));
  });
}

// Close sidebar on link click (mobile)
document.querySelectorAll('.sidebar-nav a').forEach(link => {
  link.addEventListener('click', () => {
    if (window.innerWidth <= 900) {
      sidebar.classList.remove('open');
    }
  });
});

// ── Active Section Highlighting ──
const sections = document.querySelectorAll('.docs-content h2[id], .docs-content h3[id]');
const navLinks = document.querySelectorAll('.sidebar-nav a[href^="#"]');

function updateActiveLink() {
  let current = '';
  const scrollY = window.scrollY + 100;

  sections.forEach(section => {
    if (section.offsetTop <= scrollY) {
      current = section.id;
    }
  });

  navLinks.forEach(link => {
    link.classList.remove('active');
    if (link.getAttribute('href') === '#' + current) {
      link.classList.add('active');
    }
  });
}

window.addEventListener('scroll', updateActiveLink, { passive: true });
updateActiveLink();

// ── Code Copy Buttons ──
document.querySelectorAll('.code-copy').forEach(btn => {
  btn.addEventListener('click', () => {
    const code = btn.closest('.code-block').querySelector('code').textContent;
    navigator.clipboard.writeText(code).then(() => {
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = 'Copy'; }, 2000);
    });
  });
});

// ── Scroll to Top ──
const scrollTopBtn = document.querySelector('.scroll-top');
if (scrollTopBtn) {
  window.addEventListener('scroll', () => {
    scrollTopBtn.classList.toggle('visible', window.scrollY > 400);
  }, { passive: true });

  scrollTopBtn.addEventListener('click', () => {
    window.scrollTo({ top: 0, behavior: 'smooth' });
  });
}
