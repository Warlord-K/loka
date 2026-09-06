export const THEME_KEY = "world-theme";

export const THEME_SCRIPT = `(function(){var k="${THEME_KEY}",q=window.matchMedia("(prefers-color-scheme: dark)");function a(){var m;try{m=localStorage.getItem(k)}catch(e){}var d=m==="dark"||(m!=="light"&&q.matches);document.documentElement.dataset.theme=d?"dark":"light"}a();q.addEventListener("change",a);window.addEventListener("storage",function(e){if(e.key===k)a()});window.__worldTheme=a})();`;
