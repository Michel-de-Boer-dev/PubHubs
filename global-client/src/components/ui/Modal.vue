<template>
	<div v-if="show" class="absolute h-screen w-20 sm:w-32 z-0 bg-gray-middle opacity-75"></div>
	<slot></slot>
</template>

<script setup lang="ts">
	const props = defineProps({
		show: {
			type: Boolean,
			default: false,
		},
	});
</script>
