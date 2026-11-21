// Badge and Banner share the same status vocabulary and icon/ink mapping.
import IconInfo from '~icons/ph/info-bold';
import IconSuccess from '~icons/ph/check-circle-bold';
import IconWarning from '~icons/ph/warning-bold';
import IconError from '~icons/ph/x-circle-bold';

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
