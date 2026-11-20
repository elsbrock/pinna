<!-- The mark (design/mark.svg) and the wordmark. The live ring is beside the
     satellite's name, not here. -->
<script lang="ts">
  // An image, not inline: the mark's colours are fixed, and two inline copies
  // on a page would share gradient ids.
  import markUrl from "../../../design/mark.svg?url";
</script>

<span class="pn-logo">
  <span class="mark"><img src={markUrl} alt="" width="28" height="28" /></span>
  <span class="pn-logo-word">Pinna</span>
</span>

<style>
  .mark {
    display: inline-block;
    width: 28px;
    height: 28px;
    line-height: 0;
  }
  .pn-logo-word {
    font-size: 22px;
  }
</style>
