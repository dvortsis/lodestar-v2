<script lang="ts">
	// The pattern behind "incomplete" cohorts (§10.9.3): diagonal lines in the bar's own colour, so
	// it follows the theme and needs no hex. Rendered once per page; bars reference it by id.
</script>

<svg width="0" height="0" aria-hidden="true" focusable="false" class="absolute">
	<defs>
		<pattern id="ins-hatch" patternUnits="userSpaceOnUse" width="6" height="6">
			<path d="M-1 7 7 -1M-1 1 1 -1M5 7 7 5" stroke="currentColor" stroke-width="1.5" fill="none" />
		</pattern>
	</defs>
</svg>
