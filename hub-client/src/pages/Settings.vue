<template>
	<div class="flex flex-col h-screen bg-gray-100">
		<div class="h-20 pt-4 px-4 z-10 overflow-hidden bg-blue-500 text-white">
			<h1 class="mt-4 text-2xl">{{ $t('settings.title') }}</h1>
		</div>
		<line class="m-4 mb-4"></line>
		<div class="px-4">
			<form @submit.prevent>
				<div class="flex flex-col items-center mb-4">
					<input type="file" id="avatar" accept="image/png, image/jpeg, image/svg" class="hidden" ref="file" @change="uploadAvatar($event)" />

					<div class="flex items-center">
						<Avatar :class="bgColor(color(user.user.userId))" :userName="user.user.rawDisplayName" :img="avatarUrl" class="w-32 h-32 rounded-full"></Avatar>

						<div class="flex flex-col ml-2 space-y-4">
							<!-- Added space-y-1 for vertical spacing -->
							<label for="avatar" class="font-semibold text-gray-700 cursor-pointer hover:underline">
								<Icon size="lg" type="edit" class="group-hover:block"></Icon>
							</label>

							<Icon size="lg" type="bin" class="group-hover:block" @click="removeAvatar"></Icon>
						</div>
					</div>
				</div>

				<div class="flex flex-row mb-4 items-center">
					<label class="w-1/6 font-semibold text-gray-700">{{ $t('settings.displayname') }}</label>
					<TextInput
						class="w-4/5 p-1 border rounded focus:outline-none focus:border-blue-500"
						name="displayname"
						v-model="data.displayName.value"
						:placeholder="user.user.displayName"
						@changed="updateData('displayName', $event)"
						@submit="submit"
					></TextInput>
				</div>

				<div class="flex flex-row">
					<Button @click.prevent="submit()" :disabled="!isValidated()" class="px-4 py-2 bg-blue-500 text-white rounded hover:bg-blue-700 transition duration-300 ease-in-out">{{ $t('forms.submit') }}</Button>
				</div>
			</form>
			<div v-if="message != ''" class="rounded-lg bg-red-200 p-2 mt-2 text-red-700">{{ message }}</div>
		</div>
	</div>
</template>

<script setup lang="ts">
	import { ref, onMounted } from 'vue';
	import { useUser, useDialog } from '@/store/store';
	import { useI18n } from 'vue-i18n';
	import { useFormState } from '@/composables/useFormState';
	import { fileUpload } from '@/composables/fileUpload';
	import { usePubHubs } from '@/core/pubhubsStore';
	import { useUserColor } from '@/composables/useUserColor';
	import { useMatrixFiles } from '@/composables/useMatrixFiles';

	const user = useUser();
	const { t } = useI18n();
	const { data, setData, updateData, dataIsChanged, isValidated, message, setMessage } = useFormState();
	const pubhubs = usePubHubs();
	const { imageTypes, uploadUrl, downloadUrl } = useMatrixFiles(pubhubs);
	const { color, bgColor } = useUserColor();

	const avatarUrl = ref('');

	setData({
		displayName: {
			value: '',
			validation: { required: true },
		},
	});

	async function submit() {
		if (isValidated()) {
			if (dataIsChanged('displayName')) {
				await pubhubs.changeDisplayName(data.displayName.value as string);
				setMessage(t('settings.displayname_changed', [data.displayName.value]));
				updateData('displayName', '');
			}
		}
	}

	onMounted(async () => {
		avatarUrl.value = await pubhubs.getAvatarUrl();
		if (avatarUrl.value !== '') {
			avatarUrl.value = downloadUrl + avatarUrl.value.slice(6);
		}
	});

	async function uploadAvatar(event: Event) {
		const target = event.currentTarget as HTMLInputElement;
		const file = target.files && target.files[0];
		const fileName = file && file.name;
		const dialog = useDialog();
		const accessToken = pubhubs.Auth.getAccessToken();

		fileUpload(accessToken, uploadUrl, imageTypes, event, (uri) => {
			dialog.yesno(`Do you want to upload the avatar ${fileName}?`).then((done) => {
				if (done) {
					avatarUrl.value = downloadUrl + uri.slice(6);
					pubhubs.changeAvatar(uri);
					setMessage(t('settings.avatar_changed'));
				}
			});
		});
	}
	async function removeAvatar() {
		avatarUrl.value = '';
		await pubhubs.changeAvatar(avatarUrl.value);
	}
</script>
@/composables/fileUpload
