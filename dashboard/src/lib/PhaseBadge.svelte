<script lang="ts">
  import type { Badge } from "./view.ts";

  let { badge }: { badge: Badge } = $props();
</script>

<span class="pn-badge" data-phase={badge.phase}>
  <span class="pn-badge-dot" aria-hidden="true"></span>
  {badge.word}
</span>
