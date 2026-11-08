// Bicep sample
param location string = resourceGroup().location
var storageName = 'st${uniqueString(resourceGroup().id)}'
// A storage account.
resource storage 'Microsoft.Storage/storageAccounts@2022-09-01' = {
  name: storageName
  location: location
  sku: { name: 'Standard_LRS' }
  kind: 'StorageV2'
}
module network './network.bicep' = {
  name: 'net'
}
output id string = storage.id
func greet(name string) string => 'hi ${name}'
