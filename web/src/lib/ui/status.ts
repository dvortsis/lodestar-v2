// Badge and Banner share the same status vocabulary and icon/ink mapping.
import IconInfo from '~icons/ph/info';
import IconSuccess from '~icons/ph/check-circle';
import IconWarning from '~icons/ph/warning';
import IconError from '~icons/ph/x-circle';

export const statusIcons = {
	info: IconInfo,
	success: IconSuccess,
	warning: IconWarning,
	error: IconError
};

export const statusInk = {
	info: 'text-info',
	success: 'text-success',
	warning: 'text-warning',
	error: 'text-error'
};

export const statusBorder = {
	info: 'border-info',
	success: 'border-success',
	warning: 'border-warning',
	error: 'border-error'
};
