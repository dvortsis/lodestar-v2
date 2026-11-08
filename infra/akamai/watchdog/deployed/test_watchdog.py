import importlib.util
import os
import sys
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import Mock, patch

# BEGIN ISOLATED AWS SDK TEST STUBS
# Stub AWS SDK imports exclusively while importing the Lambda under test.
# These tests never contact AWS; production Lambda code is not changed.
import types

_boto3 = types.ModuleType('boto3')
_boto3.client = lambda service, **kwargs: Mock()
_botocore = types.ModuleType('botocore')
_exceptions = types.ModuleType('botocore.exceptions')
_exceptions.ClientError = type('ClientError', (Exception,), {})
_botocore.exceptions = _exceptions
with patch.dict(sys.modules, {
    'boto3': _boto3,
    'botocore': _botocore,
    'botocore.exceptions': _exceptions,
}):
# END ISOLATED AWS SDK TEST STUBS
    spec = importlib.util.spec_from_file_location('watchdog', os.path.join(os.path.dirname(__file__), 'index.py'))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)

class WatchdogTests(unittest.TestCase):
    def setUp(self):
        self.now = datetime.now(timezone.utc)
        self.created = self.now-timedelta(hours=49)
        os.environ['MIN_CREATED_UTC'] = (self.now-timedelta(days=4)).isoformat().replace('+00:00','Z')
        os.environ['ALERT_TOPIC_ARN'] = 'arn:aws:sns:us-east-1:123456789012:topic'
        self.vm = {'id':12345,'label':'lodestar-worker-01','tags':['lodestar','test'],'region':'us-east','type':'g6-nanode-1','created':self.created.isoformat().replace('+00:00','Z')}

    def test_approved_scope(self):
        self.assertTrue(mod.scoped(self.vm))
        for changed in [ {'label':'other-project'}, {'region':'us-west'}, {'type':'g6-standard-8'}, {'tags':['lodestar']}, {'id':'12345'} ]:
            self.assertFalse(mod.scoped({**self.vm,**changed}), changed)

    def test_48h_boundary(self):
        self.assertTrue(mod.aged(self.vm,self.now))
        future={**self.vm,'created':(self.now-timedelta(hours=47)).isoformat().replace('+00:00','Z')}
        self.assertFalse(mod.aged(future,self.now))

    def test_pre_activation_excluded(self):
        os.environ['MIN_CREATED_UTC']=(self.now-timedelta(hours=30)).isoformat().replace('+00:00','Z')
        self.assertFalse(mod.aged(self.vm,self.now))

    @patch.object(mod,'token_value', return_value=None)
    @patch.object(mod,'list_paginated')
    def test_missing_token_no_external_api(self,list_fn,_token):
        result=mod.handler({},None)
        self.assertEqual(result['status'],'unconfigured')
        list_fn.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_dry_run_no_delete(self,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        result=mod.handler({'dry_run':True},None)
        self.assertEqual(result['eligible'],1)
        self.assertEqual(result['deleted'],0)
        api.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',return_value=True)
    def test_delete_only_eligible(self,confirm,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.side_effect=[self.vm,{}]
        result=mod.handler({},None)
        self.assertEqual(result['deleted'],1)
        self.assertEqual(api.call_args_list[0].args[0],'GET')
        self.assertEqual(api.call_args_list[1].args[0],'DELETE')
        confirm.assert_called_once()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_scope_change_blocks_deletion(self,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.return_value={**self.vm,'tags':['lodestar']}
        result=mod.handler({},None)
        self.assertEqual(result['deleted'],0)
        self.assertEqual(len(api.call_args_list),1)

    def test_api_path_allowlist(self):
        # Only instance paths are reachable; the guard rejects anything else
        # before a request is built, so no network call happens here.
        for bad in ['linode/instances', '//linode/instances', '/linode/../account',
                    '/networking/firewalls?page=1&page_size=100', '/account?x=1']:
            with self.assertRaises(ValueError, msg=bad):
                mod.api('GET', bad, 'thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')

if __name__=='__main__': unittest.main(verbosity=2)
