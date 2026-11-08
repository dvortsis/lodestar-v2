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

    def tearDown(self):
        os.environ.pop('TOKEN_REQUIRED', None)

    def _vms(self, n):
        return [{**self.vm, 'id': 1000 + i, 'label': 'lodestar-worker-%02d' % i} for i in range(n)]

    @patch.object(mod,'token_value', return_value=None)
    @patch.object(mod,'list_paginated')
    def test_missing_token_raises_when_required(self,list_fn,_token):
        os.environ['TOKEN_REQUIRED']='true'
        with self.assertRaises(RuntimeError):
            mod.handler({},None)
        list_fn.assert_not_called()

    def test_future_created_excluded(self):
        skewed={**self.vm,'created':(self.now+timedelta(minutes=30)).isoformat().replace('+00:00','Z')}
        self.assertFalse(mod.aged(skewed,self.now))

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    def test_more_than_ten_eligible_stops_everything(self,api,list_fn,_token):
        list_fn.return_value=self._vms(11)
        with self.assertRaises(RuntimeError):
            mod.handler({},None)
        api.assert_not_called()

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',return_value=True)
    def test_at_most_four_deletions_per_run(self,_confirm,api,list_fn,_token):
        vms=self._vms(6)
        list_fn.return_value=vms
        by_id={v['id']:v for v in vms}
        api.side_effect=lambda method,path,token,allow_404=False: by_id[int(path.rsplit('/',1)[1])] if method=='GET' else {}
        result=mod.handler({},None)
        self.assertEqual(result['eligible'],6)
        self.assertEqual(result['deleted'],4)
        self.assertEqual(sum(1 for c in api.call_args_list if c.args[0]=='DELETE'),4)

    @patch.object(mod,'token_value', return_value='thisisafaketokenxxxxxxxxxxxxxxxxxxxxxxxx')
    @patch.object(mod,'list_paginated')
    @patch.object(mod,'api')
    @patch.object(mod,'confirm_deleted',return_value=False)
    def test_unverified_deletion_raises_without_alert(self,_confirm,api,list_fn,_token):
        list_fn.return_value=[self.vm]
        api.side_effect=[self.vm,{}]
        with patch.object(mod,'SNS') as sns:
            with self.assertRaises(RuntimeError):
                mod.handler({},None)
            sns.publish.assert_not_called()

    def _page(self, page, pages, n=1):
        return {'data':[{'id':i} for i in range(n)],'page':page,'pages':pages}

    @patch.object(mod,'api')
    def test_pagination_reads_all_pages(self,api):
        api.side_effect=[self._page(1,2),self._page(2,2)]
        self.assertEqual(len(mod.list_paginated('/linode/instances','t')),2)

    @patch.object(mod,'api')
    def test_pagination_rejects_bad_responses(self,api):
        for bad in [self._page(2,2), self._page(1,21), self._page(1,0), {'data':None,'page':1,'pages':1},
                    self._page(1,1,n=501)]:
            api.side_effect=[bad]
            with self.assertRaises(RuntimeError, msg=bad.get('pages')):
                mod.list_paginated('/linode/instances','t')

    def _secret(self, value):
        client=Mock()
        client.get_secret_value.return_value={'SecretString':value}
        return patch.object(mod,'SECRETS',client)

    def test_token_value_parsing(self):
        os.environ['SECRET_ARN']='arn:aws:secretsmanager:us-east-1:123456789012:secret:x'
        good='a'*40
        with self._secret(good):
            self.assertEqual(mod.token_value(),good)
        with self._secret('{"token": "%s"}' % good):
            self.assertEqual(mod.token_value(),good)
        with self._secret(''):
            self.assertIsNone(mod.token_value())
        for bad in ['short', 'a'*20+' '+'a'*20, 'a'*513, '{"other": "x"}']:
            with self._secret(bad):
                with self.assertRaises(RuntimeError, msg=bad[:10]):
                    mod.token_value()

if __name__=='__main__': unittest.main(verbosity=2)
