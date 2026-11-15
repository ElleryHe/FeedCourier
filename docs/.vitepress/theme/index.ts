import { useRoute, type Theme } from "vitepress";
import DefaultTheme from "vitepress/theme";
import { nextTick, onMounted, watch } from "vue";
import "./custom.css";

function syncCopyrightYear() {
  if (typeof document === "undefined") return;
  const year = String(new Date().getFullYear());
  document.querySelectorAll("[data-current-year]").forEach((el) => {
    if (el.textContent !== year) {
      el.textContent = year;
    }
  });
}

export default {
  extends: DefaultTheme,
  setup() {
    const route = useRoute();
    onMounted(syncCopyrightYear);
    watch(
      () => route.path,
      () => {
        nextTick(syncCopyrightYear);
      },
    );
  },
} satisfies Theme;
